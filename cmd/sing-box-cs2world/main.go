package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"os/signal"
	"syscall"

	box "github.com/sagernet/sing-box"
	C "github.com/sagernet/sing-box/constant"
	"github.com/sagernet/sing-box/include"
	"github.com/sagernet/sing-box/log"
	"github.com/sagernet/sing-box/option"
	E "github.com/sagernet/sing/common/exceptions"
	"github.com/sagernet/sing/common/json"

	"cs2-ghost/pkg/cs2world"
	"cs2-ghost/pkg/protocol"
)

func buildCS2WorldContext(parent context.Context) context.Context {
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

func loadOptions(ctx context.Context, path string) (option.Options, error) {
	content, err := os.ReadFile(path)
	if err != nil {
		return option.Options{}, E.Cause(err, "read config at ", path)
	}
	options, err := json.UnmarshalExtendedContext[option.Options](ctx, content)
	if err != nil {
		return option.Options{}, E.Cause(err, "decode config at ", path)
	}
	return options, nil
}

func cmdRun(args []string) error {
	fs := flag.NewFlagSet("run", flag.ContinueOnError)
	var cfgPath string
	var workDir string
	fs.StringVar(&cfgPath, "c", "config.json", "configuration file path")
	fs.StringVar(&cfgPath, "config", "config.json", "configuration file path")
	fs.StringVar(&workDir, "D", "", "working directory")
	fs.StringVar(&workDir, "directory", "", "working directory")
	if err := fs.Parse(args); err != nil {
		return err
	}
	if workDir != "" {
		if err := os.Chdir(workDir); err != nil {
			return E.Cause(err, "change working directory")
		}
	}

	ctx, cancel := context.WithCancel(buildCS2WorldContext(context.Background()))
	defer cancel()

	options, err := loadOptions(ctx, cfgPath)
	if err != nil {
		return err
	}

	instance, err := box.New(box.Options{
		Context: ctx,
		Options: options,
	})
	if err != nil {
		return E.Cause(err, "create sing-box instance")
	}

	if err := instance.Start(); err != nil {
		return E.Cause(err, "start sing-box instance")
	}
	defer instance.Close()

	sigCh := make(chan os.Signal, 1)
	signal.Notify(sigCh, os.Interrupt, syscall.SIGTERM)
	<-sigCh
	return nil
}

func cmdCheck(args []string) error {
	fs := flag.NewFlagSet("check", flag.ContinueOnError)
	var cfgPath string
	fs.StringVar(&cfgPath, "c", "config.json", "configuration file path")
	fs.StringVar(&cfgPath, "config", "config.json", "configuration file path")
	if err := fs.Parse(args); err != nil {
		return err
	}

	ctx := buildCS2WorldContext(context.Background())
	options, err := loadOptions(ctx, cfgPath)
	if err != nil {
		return err
	}
	instance, err := box.New(box.Options{
		Context: ctx,
		Options: options,
	})
	if err != nil {
		return E.Cause(err, "validate sing-box configuration")
	}
	_ = instance.Close()
	fmt.Printf("Configuration %s is valid (with native cs2world protocol support).\n", cfgPath)
	return nil
}

func cmdVersion() {
	fmt.Printf("sing-box version %s (cs2world v%s)\n", C.Version, protocol.Version)
	fmt.Println("Protocols: all official sing-box protocols + cs2world (X25519 PFS + 64-bit Anti-Replay + Valve GNS + A2S Defense + AI 64-Tick Shaper)")
}

func main() {
	if len(os.Args) < 2 {
		fmt.Println("Usage: sing-box-cs2world <run|check|version> [-c config.json]")
		os.Exit(1)
	}

	subcmd := os.Args[1]
	subArgs := os.Args[2:]

	var err error
	switch subcmd {
	case "run":
		err = cmdRun(subArgs)
	case "check":
		err = cmdCheck(subArgs)
	case "version", "-v", "--version":
		cmdVersion()
		return
	default:
		// Allow direct `-c config.json` shorthand
		if subcmd == "-c" || subcmd == "--config" {
			err = cmdRun(os.Args[1:])
		} else {
			fmt.Printf("Unknown command: %s\nUsage: sing-box-cs2world <run|check|version> [-c config.json]\n", subcmd)
			os.Exit(1)
		}
	}

	if err != nil {
		log.Fatal(err)
	}
}
