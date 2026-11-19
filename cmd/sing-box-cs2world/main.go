package main

import (
	"context"
	"fmt"
	"io"
	"os"
	"os/signal"
	"path/filepath"
	"runtime"
	runtimeDebug "runtime/debug"
	"sort"
	"strconv"
	"strings"
	"syscall"
	"time"

	box "github.com/sagernet/sing-box"
	C "github.com/sagernet/sing-box/constant"
	"github.com/sagernet/sing-box/experimental/deprecated"
	"github.com/sagernet/sing-box/include"
	"github.com/sagernet/sing-box/log"
	"github.com/sagernet/sing-box/option"
	"github.com/sagernet/sing/common"
	E "github.com/sagernet/sing/common/exceptions"
	"github.com/sagernet/sing/common/json"
	"github.com/sagernet/sing/common/json/badjson"
	"github.com/sagernet/sing/service"

	"cs2-ghost/pkg/cs2world"
	"cs2-ghost/pkg/protocol"
)

type cliOptions struct {
	subcommand        string
	configPaths       []string
	configDirectories []string
	workingDir        string
	disableColor      bool
	nameOnly          bool
	help              bool
	hasConfigFlag     bool
}

type optionsEntry struct {
	path    string
	options option.Options
}

func printUsage() {
	fmt.Println("Usage: sing-box <run|check|version> [flags]")
	fmt.Println("Flags:")
	fmt.Println("  -c, --config <path>              set configuration file path (repeatable, or 'stdin')")
	fmt.Println("  -C, --config-directory <dir>     set configuration directory path (repeatable)")
	fmt.Println("  -D, --directory <dir>            set working directory")
	fmt.Println("      --disable-color              disable color output")
	fmt.Println("  -n, --name                       print version name only (for version command)")
	fmt.Println("  -v, --version                    print current version")
	fmt.Println("  -h, --help                       show help")
}

func parseCLI(args []string) (cliOptions, error) {
	var opts cliOptions
	var positionals []string

	consumeValue := func(i int, inline string, hasInline bool, flagName string) (string, int, error) {
		if hasInline {
			return inline, i, nil
		}
		if i+1 >= len(args) {
			return "", i, E.New("flag needs an argument: ", flagName)
		}
		return args[i+1], i + 1, nil
	}

	parseBoolFlag := func(inline string, hasInline bool, flagName string) (bool, error) {
		if !hasInline {
			return true, nil
		}
		v, err := strconv.ParseBool(inline)
		if err != nil {
			return false, E.Cause(err, "invalid boolean value for ", flagName)
		}
		return v, nil
	}

	for i := 0; i < len(args); i++ {
		arg := args[i]
		if arg == "--" {
			positionals = append(positionals, args[i+1:]...)
			break
		}
		if !strings.HasPrefix(arg, "-") || arg == "-" {
			positionals = append(positionals, arg)
			continue
		}

		name := arg
		inline := ""
		hasInline := false
		if eqIdx := strings.IndexByte(arg, '='); eqIdx >= 0 {
			name = arg[:eqIdx]
			inline = arg[eqIdx+1:]
			hasInline = true
		}

		switch name {
		case "-c", "--config":
			val, nextIdx, err := consumeValue(i, inline, hasInline, name)
			if err != nil {
				return opts, err
			}
			i = nextIdx
			opts.configPaths = append(opts.configPaths, val)
			opts.hasConfigFlag = true
		case "-C", "--config-directory":
			val, nextIdx, err := consumeValue(i, inline, hasInline, name)
			if err != nil {
				return opts, err
			}
			i = nextIdx
			opts.configDirectories = append(opts.configDirectories, val)
			opts.hasConfigFlag = true
		case "-D", "--directory":
			val, nextIdx, err := consumeValue(i, inline, hasInline, name)
			if err != nil {
				return opts, err
			}
			i = nextIdx
			opts.workingDir = val
		case "--disable-color", "-disable-color":
			v, err := parseBoolFlag(inline, hasInline, name)
			if err != nil {
				return opts, err
			}
			opts.disableColor = v
		case "-n", "--name":
			v, err := parseBoolFlag(inline, hasInline, name)
			if err != nil {
				return opts, err
			}
			opts.nameOnly = v
		case "-v", "--version":
			v, err := parseBoolFlag(inline, hasInline, name)
			if err != nil {
				return opts, err
			}
			if v && opts.subcommand == "" {
				opts.subcommand = "version"
			}
		case "-h", "--help":
			v, err := parseBoolFlag(inline, hasInline, name)
			if err != nil {
				return opts, err
			}
			opts.help = v
		default:
			return opts, E.New("unknown flag: ", name)
		}
	}

	if len(positionals) > 0 {
		if opts.subcommand != "" {
			return opts, E.New("unexpected argument: ", positionals[0])
		}
		opts.subcommand = positionals[0]
		if len(positionals) > 1 {
			return opts, E.New("unexpected argument: ", positionals[1])
		}
	} else if opts.subcommand == "" && opts.hasConfigFlag {
		// Allow direct `-c config.json` shorthand
		opts.subcommand = "run"
	}

	return opts, nil
}

func buildCS2WorldContext(parent context.Context) context.Context {
	parent = service.ContextWith(parent, deprecated.NewStderrManager(log.StdLogger()))

	inboundRegistry := include.InboundRegistry()
	cs2world.RegisterInbound(inboundRegistry)

	outboundRegistry := include.OutboundRegistry()
	cs2world.RegisterOutbound(outboundRegistry)

	return box.Context(
		parent,
		inboundRegistry,
		outboundRegistry,
		include.EndpointRegistry(),
		include.DNSTransportRegistry(),
		include.ServiceRegistry(),
		include.CertificateProviderRegistry(),
	)
}

func configureRuntime(opts cliOptions) (context.Context, error) {
	if opts.disableColor {
		logFactory := log.NewDefaultFactory(
			context.Background(),
			log.Formatter{BaseTime: time.Now(), DisableColors: true},
			os.Stderr,
			"",
			nil,
			false,
		)
		common.Must(logFactory.Start())
		log.SetStdLogger(logFactory.Logger())
	}

	if opts.workingDir != "" {
		if _, err := os.Stat(opts.workingDir); err != nil {
			if mkErr := os.MkdirAll(opts.workingDir, 0o777); mkErr != nil {
				return nil, E.Cause(mkErr, "create working directory")
			}
		}
		if err := os.Chdir(opts.workingDir); err != nil {
			return nil, E.Cause(err, "change working directory")
		}
	}

	return buildCS2WorldContext(context.Background()), nil
}

func readConfigAt(ctx context.Context, path string) (*optionsEntry, error) {
	var (
		content []byte
		err     error
	)
	if path == "stdin" {
		content, err = io.ReadAll(os.Stdin)
	} else {
		content, err = os.ReadFile(path)
	}
	if err != nil {
		return nil, E.Cause(err, "read config at ", path)
	}
	options, err := json.UnmarshalExtendedContext[option.Options](ctx, content)
	if err != nil {
		return nil, E.Cause(err, "decode config at ", path)
	}
	return &optionsEntry{
		path:    path,
		options: options,
	}, nil
}

func readConfigAndMerge(ctx context.Context, opts cliOptions) (option.Options, error) {
	configPaths := opts.configPaths
	if len(configPaths) == 0 && len(opts.configDirectories) == 0 {
		configPaths = []string{"config.json"}
	}

	var entries []*optionsEntry
	for _, path := range configPaths {
		entry, err := readConfigAt(ctx, path)
		if err != nil {
			return option.Options{}, err
		}
		entries = append(entries, entry)
	}

	for _, directory := range opts.configDirectories {
		dirEntries, err := os.ReadDir(directory)
		if err != nil {
			return option.Options{}, E.Cause(err, "read config directory at ", directory)
		}
		for _, dirEntry := range dirEntries {
			if !strings.HasSuffix(dirEntry.Name(), ".json") || dirEntry.IsDir() {
				continue
			}
			entry, err := readConfigAt(ctx, filepath.Join(directory, dirEntry.Name()))
			if err != nil {
				return option.Options{}, err
			}
			entries = append(entries, entry)
		}
	}

	if len(entries) == 0 {
		return option.Options{}, E.New("no configuration files found")
	}

	sort.Slice(entries, func(i, j int) bool {
		return entries[i].path < entries[j].path
	})

	if len(entries) == 1 {
		return entries[0].options, nil
	}

	var mergedMessage json.RawMessage
	var err error
	for _, entry := range entries {
		mergedMessage, err = badjson.MergeJSON(ctx, entry.options.RawMessage, mergedMessage, false)
		if err != nil {
			return option.Options{}, E.Cause(err, "merge config at ", entry.path)
		}
	}

	var mergedOptions option.Options
	if err := mergedOptions.UnmarshalJSONContext(ctx, mergedMessage); err != nil {
		return option.Options{}, E.Cause(err, "unmarshal merged config")
	}
	return mergedOptions, nil
}

func cmdRun(opts cliOptions) error {
	baseCtx, err := configureRuntime(opts)
	if err != nil {
		return err
	}

	options, err := readConfigAndMerge(baseCtx, opts)
	if err != nil {
		return err
	}
	if opts.disableColor {
		if options.Log == nil {
			options.Log = &option.LogOptions{}
		}
		options.Log.DisableColor = true
	}

	ctx, cancel := context.WithCancel(service.ExtendContext(baseCtx))
	defer cancel()

	instance, err := box.New(box.Options{
		Context: ctx,
		Options: options,
	})
	if err != nil {
		return E.Cause(err, "create sing-box instance")
	}

	if err := instance.Start(); err != nil {
		_ = instance.Close()
		return E.Cause(err, "start sing-box instance")
	}
	runtimeDebug.FreeOSMemory()

	sigCh := make(chan os.Signal, 1)
	signal.Notify(sigCh, os.Interrupt, syscall.SIGTERM)
	<-sigCh
	signal.Stop(sigCh)

	cancel()
	return instance.Close()
}

func cmdCheck(opts cliOptions) error {
	baseCtx, err := configureRuntime(opts)
	if err != nil {
		return err
	}

	options, err := readConfigAndMerge(baseCtx, opts)
	if err != nil {
		return err
	}

	ctx, cancel := context.WithCancel(service.ExtendContext(baseCtx))
	defer cancel()

	instance, err := box.New(box.Options{
		Context: ctx,
		Options: options,
	})
	if err != nil {
		return E.Cause(err, "validate sing-box configuration")
	}
	_ = instance.Close()
	fmt.Println("Configuration is valid (with native cs2world protocol support).")
	return nil
}

func cmdVersion(nameOnly bool) {
	if nameOnly {
		os.Stdout.WriteString(C.Version + "\n")
		return
	}

	var out strings.Builder
	out.WriteString("sing-box version " + C.Version + "\n\n")
	out.WriteString("Environment: " + runtime.Version() + " " + runtime.GOOS + "/" + runtime.GOARCH + "\n")

	var tags string
	if debugInfo, loaded := runtimeDebug.ReadBuildInfo(); loaded {
		for _, setting := range debugInfo.Settings {
			if setting.Key == "-tags" {
				tags = setting.Value
				break
			}
		}
	}
	if tags != "" {
		out.WriteString("Tags: " + tags + "\n")
	}

	if C.CGO_ENABLED {
		out.WriteString("CGO: enabled\n")
	} else {
		out.WriteString("CGO: disabled\n")
	}

	out.WriteString(fmt.Sprintf(
		"Protocols: all official sing-box protocols + cs2world (v%s: X25519 PFS + 64-bit Anti-Replay + Valve GNS + A2S Defense + AI 64-Tick Shaper)\n",
		protocol.Version,
	))
	os.Stdout.WriteString(out.String())
}

func main() {
	if len(os.Args) < 2 {
		printUsage()
		os.Exit(1)
	}

	opts, err := parseCLI(os.Args[1:])
	if err != nil {
		log.Fatal(err)
	}
	if opts.help {
		printUsage()
		return
	}

	switch opts.subcommand {
	case "run":
		err = cmdRun(opts)
	case "check":
		err = cmdCheck(opts)
	case "version":
		cmdVersion(opts.nameOnly)
		return
	case "":
		printUsage()
		os.Exit(1)
	default:
		fmt.Printf("Unknown command: %s\n", opts.subcommand)
		printUsage()
		os.Exit(1)
	}

	if err != nil {
		log.Fatal(err)
	}
}
