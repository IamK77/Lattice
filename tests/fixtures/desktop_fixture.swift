// A disposable, non-networked native window for desktop integration tests.
import AppKit

final class Fixture: NSObject, NSApplicationDelegate, NSTextFieldDelegate {
    var window: NSWindow!
    var field: NSTextField!
    func report(_ value: String) {
        FileHandle.standardOutput.write((value + "\n").data(using: .utf8)!)
    }
    func applicationDidFinishLaunching(_ notification: Notification) {
        window = NSWindow(contentRect: NSRect(x: 100, y: 100, width: 480, height: 240),
                          styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.title = "Lattice Desktop Verification"
        field = NSTextField(frame: NSRect(x: 30, y: 100, width: 420, height: 40))
        field.placeholderString = "Owned test field"
        field.font = NSFont.systemFont(ofSize: 18)
        field.delegate = self
        window.contentView!.addSubview(field)
        window.orderFront(nil)
        window.makeFirstResponder(field)
        report("READY")
    }
    func controlTextDidChange(_ notification: Notification) {
        report("TEXT:" + field.stringValue)
    }
}
let app = NSApplication.shared
app.setActivationPolicy(.regular)
let fixture = Fixture()
app.delegate = fixture
app.run()
