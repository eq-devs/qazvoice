import AVFoundation
import Flutter
import UIKit

/// iOS side of qazvoice: downloads, audio I/O, decoding, device facts.
/// Same channel protocol as the Android plugin.
public class QazvoicePlugin: NSObject, FlutterPlugin, FlutterStreamHandler {
  private var eventSink: FlutterEventSink?
  private var players: [Int: PcmPlayer] = [:]
  private var nextPlayer = 1
  private var decodeCancelled: [Int: Bool] = [:]
  private let lock = NSLock()
  private var mic: Mic?
  private let io = DispatchQueue(label: "kz.qazvoice.io", qos: .userInitiated, attributes: .concurrent)

  public static func register(with registrar: FlutterPluginRegistrar) {
    let instance = QazvoicePlugin()
    let channel = FlutterMethodChannel(name: "kz.qazvoice/main", binaryMessenger: registrar.messenger())
    registrar.addMethodCallDelegate(instance, channel: channel)
    FlutterEventChannel(name: "kz.qazvoice/events", binaryMessenger: registrar.messenger()).setStreamHandler(instance)
    FlutterEventChannel(name: "kz.qazvoice/mic", binaryMessenger: registrar.messenger())
      .setStreamHandler(MicHandler(plugin: instance))
    registrar.addApplicationDelegate(instance)
    NotificationCenter.default.addObserver(instance, selector: #selector(memoryWarning),
                                           name: UIApplication.didReceiveMemoryWarningNotification, object: nil)
  }

  @objc private func memoryWarning() { send(["type": "trimMemory", "level": 80]) }

  public func onListen(withArguments arguments: Any?, eventSink events: @escaping FlutterEventSink) -> FlutterError? {
    eventSink = events
    return nil
  }

  public func onCancel(withArguments arguments: Any?) -> FlutterError? {
    eventSink = nil
    return nil
  }

  func send(_ e: [String: Any]) {
    DispatchQueue.main.async { self.eventSink?(e) }
  }

  public func application(_ application: UIApplication, handleEventsForBackgroundURLSession identifier: String,
                          completionHandler: @escaping () -> Void) -> Bool {
    guard identifier.hasPrefix("kz.qazvoice.downloads.") else { return false }
    DownloadManager.shared.backgroundCompletion = completionHandler
    DownloadManager.shared.initialize()
    return true
  }

  private func bg(_ result: @escaping FlutterResult, _ block: @escaping () throws -> Any?) {
    io.async {
      do {
        let v = try block()
        DispatchQueue.main.async { result(v) }
      } catch let e as DownloadError {
        DispatchQueue.main.async { result(FlutterError(code: e.kind, message: e.message, details: e.extras)) }
      } catch {
        DispatchQueue.main.async { result(FlutterError(code: "audio", message: "\(error)", details: nil)) }
      }
    }
  }

  public func handle(_ call: FlutterMethodCall, result: @escaping FlutterResult) {
    let args = call.arguments as? [String: Any] ?? [:]
    func id() throws -> String { try JobSpec.checkId(args["id"] as? String) }
    let dm = DownloadManager.shared
    switch call.method {
    case "init":
      bg(result) {
        dm.initialize()
        dm.listener = { [weak self] e in self?.send(e) }
        return ["modelsDir": dm.store.root.path, "states": dm.states()]
      }
    case "device": bg(result) { self.device() }
    case "modelState": bg(result) { dm.state(try id()) }
    case "download":
      bg(result) { try dm.enqueue(try JobSpec.from(args)); return nil }
    case "pause": bg(result) { dm.pause(try id()); return nil }
    case "resume": bg(result) { dm.resume(try id()); return nil }
    case "cancel": bg(result) { dm.cancel(try id()); return nil }
    case "delete": bg(result) { dm.delete(try id()); return nil }
    case "storageUsed": bg(result) { dm.usedBytes() }
    case "tempDir": result(NSTemporaryDirectory())
    case "requestNotificationPermission": result(true)
    case "requestMicrophonePermission":
      guard Bundle.main.object(forInfoDictionaryKey: "NSMicrophoneUsageDescription") != nil else {
        result(FlutterError(code: "permission", message: "add NSMicrophoneUsageDescription to Info.plist",
                            details: ["permission": "microphone"]))
        return
      }
      AVAudioSession.sharedInstance().requestRecordPermission { ok in DispatchQueue.main.async { result(ok) } }
    case "playerCreate":
      let rate = (args["rate"] as? NSNumber)?.intValue ?? 22050
      lock.lock(); let pid = nextPlayer; nextPlayer += 1; lock.unlock()
      do {
        let p = try PcmPlayer(rate: rate) { [weak self] frames, level in
          self?.send(["type": "player", "id": pid, "frames": frames, "level": level])
        }
        lock.lock(); players[pid] = p; lock.unlock()
        result(pid)
      } catch {
        result(FlutterError(code: "audio", message: "cannot open audio output: \(error)", details: nil))
      }
    case "playerWrite":
      let pid = (args["id"] as? NSNumber)?.intValue ?? -1
      lock.lock(); let p = players[pid]; lock.unlock()
      guard let p = p, let data = (args["pcm"] as? FlutterStandardTypedData)?.data else { result(nil); return }
      p.write(data) { DispatchQueue.main.async { result(nil) } }
    case "playerFinish":
      let pid = (args["id"] as? NSNumber)?.intValue ?? -1
      lock.lock(); let p = players[pid]; lock.unlock()
      guard let p = p else { result(nil); return }
      p.finish {
        self.lock.lock(); self.players[pid] = nil; self.lock.unlock()
        DispatchQueue.main.async { result(nil) }
      }
    case "playerStop":
      let pid = (args["id"] as? NSNumber)?.intValue ?? -1
      lock.lock(); let p = players.removeValue(forKey: pid); lock.unlock()
      p?.stop()
      result(nil)
    case "decode":
      let token = (args["token"] as? NSNumber)?.intValue ?? 0
      lock.lock(); decodeCancelled[token] = false; lock.unlock()
      bg(result) {
        defer { self.lock.lock(); self.decodeCancelled[token] = nil; self.lock.unlock() }
        let r = try AudioFiles.decode(
          input: args["input"] as? String ?? "", output: URL(fileURLWithPath: args["output"] as? String ?? ""),
          cancelled: { self.lock.lock(); defer { self.lock.unlock() }; return self.decodeCancelled[token] == true },
          progress: { f in self.send(["type": "decode", "token": token, "fraction": f]) })
        return ["rate": r.rate, "channels": r.channels, "durationMs": r.durationMs, "bytes": r.bytes]
      }
    case "cancelDecode":
      let token = (args["token"] as? NSNumber)?.intValue ?? 0
      lock.lock(); if decodeCancelled[token] != nil { decodeCancelled[token] = true }; lock.unlock()
      result(nil)
    case "encodeM4a":
      bg(result) {
        try AudioFiles.encodeM4a(input: URL(fileURLWithPath: args["input"] as? String ?? ""),
                                 rate: (args["rate"] as? NSNumber)?.intValue ?? 22050,
                                 output: URL(fileURLWithPath: args["output"] as? String ?? ""))
        return nil
      }
    default:
      result(FlutterMethodNotImplemented)
    }
  }

  private func device() -> [String: Any] {
    var free: Int64 = -1
    let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
    if let v = try? support.resourceValues(forKeys: [.volumeAvailableCapacityForImportantUsageKey]) {
      free = v.volumeAvailableCapacityForImportantUsage ?? -1
    }
    var sys = utsname()
    uname(&sys)
    let machine = withUnsafePointer(to: &sys.machine) {
      $0.withMemoryRebound(to: CChar.self, capacity: 1) { String(cString: $0) }
    }
    return [
      "abis": ["arm64"],
      "totalRam": Int64(ProcessInfo.processInfo.physicalMemory),
      "availRam": Int64(os_proc_available_memory()),
      "lowRam": false,
      "freeBytes": free,
      "sdk": ProcessInfo.processInfo.operatingSystemVersion.majorVersion,
      "model": "Apple \(machine)",
    ]
  }

  fileprivate func startMic(rate: Int, sink: @escaping FlutterEventSink) {
    guard AVAudioSession.sharedInstance().recordPermission == .granted else {
      sink(FlutterError(code: "permission", message: "microphone permission not granted", details: ["permission": "microphone"]))
      sink(FlutterEndOfEventStream)
      return
    }
    mic?.stop()
    let m = Mic(rate: rate) { data in
      DispatchQueue.main.async { sink(FlutterStandardTypedData(bytes: data)) }
    }
    do {
      try m.start()
      mic = m
    } catch {
      sink(FlutterError(code: "audio", message: "\(error)", details: nil))
    }
  }

  fileprivate func stopMic() {
    mic?.stop()
    mic = nil
  }
}

private final class MicHandler: NSObject, FlutterStreamHandler {
  weak var plugin: QazvoicePlugin?
  init(plugin: QazvoicePlugin) { self.plugin = plugin }

  func onListen(withArguments arguments: Any?, eventSink events: @escaping FlutterEventSink) -> FlutterError? {
    let rate = ((arguments as? [String: Any])?["rate"] as? NSNumber)?.intValue ?? 16000
    plugin?.startMic(rate: rate, sink: events)
    return nil
  }

  func onCancel(withArguments arguments: Any?) -> FlutterError? {
    plugin?.stopMic()
    return nil
  }
}
