import AppKit
import CS2World

private func receive(_ context: UnsafeMutableRawPointer?, _ bytes: UnsafePointer<UInt8>?, _ length: Int) {
    guard let context = context, let bytes = bytes else { return }
    let text = String(decoding: UnsafeBufferPointer(start: bytes, count: length), as: UTF8.self)
    let owner = Unmanaged<Client>.fromOpaque(context).takeUnretainedValue()
    DispatchQueue.main.async { owner.event(text) }
}

@MainActor
private final class Client: NSObject, NSApplicationDelegate, NSWindowDelegate {
    private let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 620, height: 400),
                                  styleMask: [.titled, .closable, .miniaturizable, .resizable],
                                  backing: .buffered, defer: false)
    private let config = NSTextView()
    private let status = NSTextField(labelWithString: "Disconnected")
    private let mode = NSButton(checkboxWithTitle: "Route system traffic through utun", target: nil, action: nil)
    private var handle: UInt64 = 0
    private var startId: Int64 = 0
    private var authId: Int64 = 0
    private var tunId: Int64 = 0
    private var stopId: Int64 = 0
    private var verificationReceipt: URL?
    private var verifiedStop = false

    func applicationDidFinishLaunching(_ note: Notification) {
        let arguments = ProcessInfo.processInfo.arguments
        if let flag = arguments.firstIndex(of: "--verify-launch"), flag + 1 < arguments.count {
            verificationReceipt = URL(fileURLWithPath: arguments[flag + 1])
        }
        handle = cs2w_create()
        let coreReady = handle != 0 && cs2w_abi_version() == UInt32(CS2W_ABI_VERSION)
            && cs2w_set_event_callback(handle, receive, Unmanaged.passUnretained(self).toOpaque()) == 0
        status.stringValue = coreReady ? "Paste a cs2world:// link or open a client JSON file." : "Could not initialize the native core"
        let menu = NSMenu()
        let appMenu = NSMenu(title: "CS2World")
        appMenu.addItem(withTitle: "Quit CS2World", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
        menu.addItem(withTitle: "CS2World", action: nil, keyEquivalent: "").submenu = appMenu
        let edit = NSMenu(title: "Edit")
        edit.addItem(withTitle: "Cut", action: #selector(NSText.cut(_:)), keyEquivalent: "x")
        edit.addItem(withTitle: "Copy", action: #selector(NSText.copy(_:)), keyEquivalent: "c")
        edit.addItem(withTitle: "Paste", action: #selector(NSText.paste(_:)), keyEquivalent: "v")
        edit.addItem(withTitle: "Select All", action: #selector(NSText.selectAll(_:)), keyEquivalent: "a")
        menu.addItem(withTitle: "Edit", action: nil, keyEquivalent: "").submenu = edit
        NSApp.mainMenu = menu
        window.isReleasedWhenClosed = false
        window.title = "CS2World"; window.delegate = self
        let stack = NSStackView()
        stack.orientation = .vertical; stack.spacing = 10; stack.alignment = .leading
        stack.edgeInsets = NSEdgeInsets(top: 16, left: 16, bottom: 16, right: 16)
        stack.translatesAutoresizingMaskIntoConstraints = false
        let scroll = NSScrollView(); scroll.hasVerticalScroller = true; scroll.borderType = .bezelBorder
        config.isRichText = false; config.isAutomaticQuoteSubstitutionEnabled = false
        config.font = .monospacedSystemFont(ofSize: 12, weight: .regular)
        config.isVerticallyResizable = true
        config.isHorizontallyResizable = false
        config.autoresizingMask = [.width]
        config.minSize = NSSize(width: 0, height: 120)
        config.maxSize = NSSize(width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)
        config.textContainer?.widthTracksTextView = true
        scroll.documentView = config
        let buttons = NSStackView(views: [
            NSButton(title: "Open JSON…", target: self, action: #selector(openFile)),
            NSButton(title: "Connect", target: self, action: #selector(start)),
            NSButton(title: "Disconnect", target: self, action: #selector(stop))
        ])
        [scroll, mode, buttons, status].forEach { stack.addArrangedSubview($0) }
        let content = window.contentView!; content.addSubview(stack)
        NSLayoutConstraint.activate([stack.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: content.trailingAnchor), stack.topAnchor.constraint(equalTo: content.topAnchor),
            stack.bottomAnchor.constraint(equalTo: content.bottomAnchor), scroll.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -32),
            scroll.heightAnchor.constraint(greaterThanOrEqualToConstant: 120)])
        window.center(); window.makeKeyAndOrderFront(nil); NSApp.activate(ignoringOtherApps: true)
        if verificationReceipt != nil {
            // Exercise the same AppKit window, command queue and callback path as the UI.
            guard coreReady && window.isVisible else { NSApp.terminate(nil); return }
            config.string = #"{"inbounds":[{"type":"mixed","listen":"127.0.0.1","listen_port":0}],"outbounds":[{"type":"direct"}]}"#
            start()
            DispatchQueue.main.asyncAfter(deadline: .now() + 10) { NSApp.terminate(nil) }
        }
    }
    @objc private func openFile() {
        let panel = NSOpenPanel(); panel.allowsMultipleSelection = false
        if panel.runModal() == .OK, let url = panel.url {
            do { config.string = try String(contentsOf: url, encoding: .utf8) }
            catch { status.stringValue = error.localizedDescription }
        }
    }
    @objc private func start() {
        let bytes = Array(config.string.utf8)
        startId = bytes.withUnsafeBufferPointer { cs2w_start(handle, $0.baseAddress, $0.count) }
        status.stringValue = startId > 0 ? "Starting" : "Could not queue start."
    }
    @objc private func stop() {
        stopId = cs2w_stop(handle)
        status.stringValue = stopId > 0 ? "Disconnecting" : "Could not queue stop."
    }
    func event(_ text: String) {
        guard let object = try? JSONSerialization.jsonObject(with: Data(text.utf8)) as? [String: Any],
              let ticket = object["command"] as? NSNumber, let ok = object["ok"] as? Bool else { return }
        let id = ticket.int64Value
        if id == 0 { return }
        if !ok {
            status.stringValue = object["detail"] as? String ?? "Connection failed"
            if verificationReceipt != nil { NSApp.terminate(nil); return }
            if id == authId || id == tunId { _ = cs2w_stop(handle) }
            return
        }
        switch id {
        case startId:
            if verificationReceipt != nil {
                guard let detail = object["detail"] as? [String: Any],
                      detail["state"] as? String == "running",
                      let listen = detail["listen"] as? String,
                      listen.hasPrefix("127.0.0.1:"),
                      let port = UInt16(listen.dropFirst("127.0.0.1:".count)), port > 0
                else { NSApp.terminate(nil); return }
                stop()
                return
            }
            status.stringValue = "Authenticating"; authId = cs2w_preconnect(handle)
        case authId:
            if mode.state == .on { status.stringValue = "Starting utun"; tunId = cs2w_start_desktop_tun(handle) }
            else {status.stringValue = "Connected · Mixed proxy ready"}
        case tunId: status.stringValue = "Connected · utun active"
        case stopId:
            status.stringValue = "Disconnected"
            if verificationReceipt != nil {
                verifiedStop = object["detail"] as? String == "stopped"
                NSApp.terminate(nil)
            }
        default: break
        }
    }
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }
    func applicationWillTerminate(_ note: Notification) {
        let drained = cs2w_set_event_callback(handle, nil, nil)
        let destroyed = cs2w_destroy(handle)
        if let receipt = verificationReceipt, verifiedStop && drained == 0 && destroyed == 0 {
            try? "window,abi,start,stop,destroy\n".write(to: receipt, atomically: true, encoding: .utf8)
        }
    }
}
let app = NSApplication.shared
private let client = Client()
app.setActivationPolicy(.regular); app.delegate = client; app.run()
