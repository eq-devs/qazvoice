import AVFoundation
import Foundation

/// One owner of the shared AVAudioSession for players and the microphone:
/// the session is deactivated only when the last user releases it, and the
/// category is chosen for everything currently active.
enum SessionOwner {
  private static let lock = NSLock()
  private static var players = 0
  private static var mics = 0

  private static func apply() {
    let s = AVAudioSession.sharedInstance()
    if mics > 0 {
      // Recording (with or without playback): no voice processing, speaker output.
      let mode: AVAudioSession.Mode = players > 0 ? .default : .measurement
      try? s.setCategory(.playAndRecord, mode: mode, options: [.defaultToSpeaker, .allowBluetooth])
    } else if players > 0 {
      try? s.setCategory(.playback, mode: .spokenAudio, options: [.duckOthers])
    }
  }

  static func acquire(mic: Bool) throws {
    lock.lock(); defer { lock.unlock() }
    let wasIdle = players + mics == 0
    if mic { mics += 1 } else { players += 1 }
    // Change the category only when it is not already what we need
    // (switching it mid-playback interrupts the running engine).
    if wasIdle || (mic && mics == 1) { apply() }
    try AVAudioSession.sharedInstance().setActive(true)
  }

  static func release(mic: Bool) {
    lock.lock(); defer { lock.unlock() }
    if mic { mics = max(0, mics - 1) } else { players = max(0, players - 1) }
    if players + mics == 0 {
      try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
    }
  }
}

/// Streams 16-bit mono PCM through AVAudioEngine. `write` calls back once fewer
/// than `maxQueuedSeconds` of audio wait (back-pressure for the synthesizer);
/// `finish` calls back when everything has been heard. Reports position/level ~20×/s.
final class PcmPlayer {
  private let engine = AVAudioEngine()
  private let node = AVAudioPlayerNode()
  private let format: AVAudioFormat
  private let rate: Double
  private let lock = NSLock()
  private var scheduled: AVAudioFramePosition = 0
  private var played: AVAudioFramePosition = 0
  private var queuedFrames: AVAudioFramePosition = 0
  private var waiters: [() -> Void] = []
  private var finishCallback: (() -> Void)?
  private var levels: [Float] = [] // per 512 frames
  private var timer: DispatchSourceTimer?
  private var stopped = false
  private let maxQueued: AVAudioFramePosition
  private let onPosition: (Int64, Double) -> Void
  private var holdsSession = false
  private var observers: [NSObjectProtocol] = []
  /// Node time keeps running while nothing is queued; this maps it back to audio heard.
  private var timeOffset: Int64 = 0

  init(rate: Int, onPosition: @escaping (Int64, Double) -> Void) throws {
    self.rate = Double(rate)
    self.onPosition = onPosition
    self.maxQueued = AVAudioFramePosition(rate * 3)
    guard let f = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: Double(rate), channels: 1, interleaved: false) else {
      throw DownloadError("audio", "unsupported sample rate \(rate)")
    }
    format = f
    try? SessionOwner.acquire(mic: false)
    holdsSession = true
    engine.attach(node)
    engine.connect(node, to: engine.mainMixerNode, format: format)
    do {
      try engine.start()
    } catch {
      SessionOwner.release(mic: false)
      holdsSession = false
      throw error
    }
    node.play()
    // A call, Siri or a route change stops the engine: end playback so no
    // caller waits forever for buffers that will never play.
    let nc = NotificationCenter.default
    observers.append(nc.addObserver(forName: AVAudioSession.interruptionNotification, object: nil, queue: nil) { [weak self] n in
      let t = (n.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt).flatMap(AVAudioSession.InterruptionType.init)
      if t == .began { DispatchQueue.global().async { self?.stop() } }
    })
    observers.append(nc.addObserver(forName: .AVAudioEngineConfigurationChange, object: engine, queue: nil) { [weak self] _ in
      DispatchQueue.global().async { self?.stop() }
    })
    let t = DispatchSource.makeTimerSource(queue: DispatchQueue.global(qos: .userInitiated))
    t.schedule(deadline: .now(), repeating: .milliseconds(50))
    t.setEventHandler { [weak self] in self?.report() }
    t.resume()
    timer = t
  }

  private func headFrames() -> Int64 {
    guard let nt = node.lastRenderTime, let pt = node.playerTime(forNodeTime: nt) else { return 0 }
    return max(0, Int64(pt.sampleTime))
  }

  private func report() {
    // Ask the node first, without our lock: its completion callbacks take our
    // lock while holding the node's own (lock-order inversion otherwise).
    let rendered = headFrames()
    lock.lock()
    let head = max(Int64(played), min(rendered - timeOffset, Int64(scheduled)))
    let level = levels.indices.contains(Int(head / 512)) ? levels[Int(head / 512)] : 0
    let done = stopped
    lock.unlock()
    if !done { onPosition(head, Double(level)) }
  }

  func write(_ data: Data, done: @escaping () -> Void) {
    let n = data.count / 2
    guard n > 0, let buf = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: AVAudioFrameCount(n)) else { done(); return }
    buf.frameLength = AVAudioFrameCount(n)
    let out = buf.floatChannelData![0]
    data.withUnsafeBytes { raw in
      let s = raw.bindMemory(to: Int16.self)
      for i in 0..<n { out[i] = Float(Int16(littleEndian: s[i])) / 32768 }
    }
    let raw = headFrames()
    lock.lock()
    if stopped { lock.unlock(); done(); return }
    if queuedFrames == 0 {
      // The queue ran dry (synthesis was slower than playback): restart the clock here.
      timeOffset = raw - Int64(played)
    }
    var i = 0
    while i < n {
      var peak: Float = 0
      for j in stride(from: i, to: min(n, i + 512), by: 4) { peak = max(peak, abs(out[j])) }
      levels.append(peak)
      i += 512
    }
    scheduled += AVAudioFramePosition(n)
    queuedFrames += AVAudioFramePosition(n)
    let full = queuedFrames > maxQueued
    if full { waiters.append(done) }
    lock.unlock()
    node.scheduleBuffer(buf, completionCallbackType: .dataPlayedBack) { [weak self] _ in self?.played(n) }
    if !full { done() }
  }

  private func played(_ n: Int) {
    lock.lock()
    queuedFrames -= AVAudioFramePosition(n)
    played += AVAudioFramePosition(n)
    var ws: [() -> Void] = []
    if queuedFrames <= maxQueued { ws = waiters; waiters.removeAll() }
    var fin: (() -> Void)?
    if queuedFrames <= 0, let f = finishCallback { fin = f; finishCallback = nil }
    lock.unlock()
    ws.forEach { $0() }
    // Never stop the node from inside its own completion callback (deadlock).
    if let f = fin { DispatchQueue.global(qos: .userInitiated).async { self.shutdown(); f() } }
  }

  func finish(_ done: @escaping () -> Void) {
    lock.lock()
    if stopped || queuedFrames <= 0 { lock.unlock(); shutdown(); done(); return }
    finishCallback = done
    lock.unlock()
  }

  func stop() {
    lock.lock()
    let ws = waiters; waiters.removeAll()
    let fin = finishCallback; finishCallback = nil
    lock.unlock()
    shutdown()
    ws.forEach { $0() }
    fin?()
  }

  private func shutdown() {
    lock.lock()
    if stopped { lock.unlock(); return }
    stopped = true
    lock.unlock()
    timer?.cancel(); timer = nil
    onPosition(Int64(played), 0)
    observers.forEach { NotificationCenter.default.removeObserver($0) }
    observers.removeAll()
    node.stop()
    engine.stop()
    if holdsSession { holdsSession = false; SessionOwner.release(mic: false) }
  }
}

/// Microphone capture for recognition: 16-bit mono at `rate`, 100 ms chunks.
final class Mic {
  private let engine = AVAudioEngine()
  private var converter: AVAudioConverter?
  private var pending = Data()
  private let rate: Double
  private let onChunk: (Data) -> Void
  private let onError: (String) -> Void
  private var holdsSession = false
  private var observer: NSObjectProtocol?

  init(rate: Int, onChunk: @escaping (Data) -> Void, onError: @escaping (String) -> Void) {
    self.rate = Double(rate)
    self.onChunk = onChunk
    self.onError = onError
  }

  func start() throws {
    try SessionOwner.acquire(mic: true)
    holdsSession = true
    observer = NotificationCenter.default.addObserver(forName: AVAudioSession.interruptionNotification, object: nil, queue: nil) { [weak self] n in
      let t = (n.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt).flatMap(AVAudioSession.InterruptionType.init)
      if t == .began { self?.onError("recording interrupted") }
    }
    let input = engine.inputNode
    let inFormat = input.outputFormat(forBus: 0)
    guard inFormat.sampleRate > 0,
          let outFormat = AVAudioFormat(commonFormat: .pcmFormatInt16, sampleRate: rate, channels: 1, interleaved: true),
          let conv = AVAudioConverter(from: inFormat, to: outFormat) else {
      throw DownloadError("audio", "microphone unavailable")
    }
    converter = conv
    let chunkBytes = Int(rate) / 10 * 2
    input.installTap(onBus: 0, bufferSize: 4096, format: inFormat) { [weak self] buf, _ in
      guard let self = self, let conv = self.converter else { return }
      let cap = AVAudioFrameCount(Double(buf.frameLength) * self.rate / inFormat.sampleRate + 32)
      guard let out = AVAudioPCMBuffer(pcmFormat: outFormat, frameCapacity: cap) else { return }
      var given = false
      var err: NSError?
      conv.convert(to: out, error: &err) { _, status in
        if given { status.pointee = .noDataNow; return nil }
        given = true
        status.pointee = .haveData
        return buf
      }
      guard err == nil, out.frameLength > 0, let p = out.int16ChannelData?[0] else { return }
      self.pending.append(Data(bytes: p, count: Int(out.frameLength) * 2))
      while self.pending.count >= chunkBytes {
        self.onChunk(self.pending.prefix(chunkBytes))
        self.pending.removeFirst(chunkBytes)
      }
    }
    engine.prepare()
    do {
      try engine.start()
    } catch {
      stop()
      throw error
    }
  }

  func stop() {
    if let o = observer { NotificationCenter.default.removeObserver(o); observer = nil }
    engine.inputNode.removeTap(onBus: 0)
    engine.stop()
    if holdsSession { holdsSession = false; SessionOwner.release(mic: true) }
  }
}

enum AudioFiles {
  struct Decoded { let rate: Int; let channels: Int; let durationMs: Int64; let bytes: Int64 }

  /// Any audio/video file AVFoundation can read → interleaved s16le PCM at the source rate.
  static func decode(input: String, output: URL, cancelled: () -> Bool, progress: (Double) -> Void) throws -> Decoded {
    let url: URL
    if input.hasPrefix("file://") {
      guard let u = URL(string: input) ?? URL(string: input.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? "") else {
        throw DownloadError("audio", "bad file URL")
      }
      url = u
    } else {
      url = URL(fileURLWithPath: input)
    }
    let asset = AVURLAsset(url: url)
    guard let track = asset.tracks(withMediaType: .audio).first else { throw DownloadError("audio", "no audio track") }
    let duration = CMTimeGetSeconds(asset.duration)
    let reader = try AVAssetReader(asset: asset)
    let settings: [String: Any] = [AVFormatIDKey: kAudioFormatLinearPCM, AVLinearPCMBitDepthKey: 16,
                                   AVLinearPCMIsFloatKey: false, AVLinearPCMIsBigEndianKey: false,
                                   AVLinearPCMIsNonInterleaved: false]
    let out = AVAssetReaderTrackOutput(track: track, outputSettings: settings)
    reader.add(out)
    guard reader.startReading() else { throw DownloadError("audio", "cannot read \(url.lastPathComponent)") }
    // Decoded PCM can be far larger than the file: check there is room.
    if let desc = track.formatDescriptions.first,
       let asbd = CMAudioFormatDescriptionGetStreamBasicDescription(desc as! CMAudioFormatDescription)?.pointee,
       duration.isFinite {
      let estimate = Int64(duration * asbd.mSampleRate * Double(asbd.mChannelsPerFrame) * 2) + (64 << 20)
      let free = (try? output.deletingLastPathComponent().resourceValues(forKeys: [.volumeAvailableCapacityForImportantUsageKey]))?
        .volumeAvailableCapacityForImportantUsage ?? Int64.max
      if estimate > free {
        throw DownloadError("storage", "not enough storage to decode", ["needed": estimate, "available": free])
      }
    }
    FileManager.default.createFile(atPath: output.path, contents: nil)
    let h = try FileHandle(forWritingTo: output)
    defer { try? h.close() }
    var rate = 0, channels = 0
    var bytes: Int64 = 0
    var lastProgress = -1.0
    do {
      while let sb = out.copyNextSampleBuffer() {
        if cancelled() { reader.cancelReading(); throw DownloadError("cancelled", "cancelled") }
        if rate == 0, let fd = CMSampleBufferGetFormatDescription(sb),
           let a = CMAudioFormatDescriptionGetStreamBasicDescription(fd)?.pointee {
          rate = Int(a.mSampleRate); channels = Int(a.mChannelsPerFrame)
        }
        if let block = CMSampleBufferGetDataBuffer(sb) {
          var len = 0
          var ptr: UnsafeMutablePointer<CChar>?
          if CMBlockBufferGetDataPointer(block, atOffset: 0, lengthAtOffsetOut: nil, totalLengthOut: &len, dataPointerOut: &ptr) == kCMBlockBufferNoErr,
             let p = ptr, CMBlockBufferIsRangeContiguous(block, atOffset: 0, length: len) {
            h.write(Data(bytes: p, count: len))
          } else {
            var d = Data(count: CMBlockBufferGetDataLength(block))
            d.withUnsafeMutableBytes { _ = CMBlockBufferCopyDataBytes(block, atOffset: 0, dataLength: $0.count, destination: $0.baseAddress!) }
            h.write(d)
            len = d.count
          }
          bytes += Int64(len)
        }
        let t = CMTimeGetSeconds(CMSampleBufferGetPresentationTimeStamp(sb))
        if duration > 0, t - lastProgress >= 0.5 { lastProgress = t; progress(min(1, t / duration)) }
      }
      if reader.status == .failed { throw DownloadError("audio", reader.error?.localizedDescription ?? "decode failed") }
      if bytes == 0 || rate == 0 { throw DownloadError("audio", "no audio decoded") }
    } catch {
      try? FileManager.default.removeItem(at: output)
      throw error
    }
    progress(1)
    return Decoded(rate: rate, channels: channels, durationMs: Int64(duration * 1000), bytes: bytes)
  }

  /// Raw s16le mono PCM → AAC in an .m4a file.
  static func encodeM4a(input: URL, rate: Int, output: URL) throws {
    // AVAudioFile picks the container from the extension: keep ".m4a" last.
    let tmp = output.deletingPathExtension().appendingPathExtension("tmp.m4a")
    try? FileManager.default.removeItem(at: tmp)
    let settings: [String: Any] = [AVFormatIDKey: kAudioFormatMPEG4AAC, AVSampleRateKey: rate,
                                   AVNumberOfChannelsKey: 1, AVEncoderBitRateKey: 64000]
    guard let fmt = AVAudioFormat(commonFormat: .pcmFormatInt16, sampleRate: Double(rate), channels: 1, interleaved: true) else {
      throw DownloadError("audio", "bad rate")
    }
    do {
      let file = try AVAudioFile(forWriting: tmp, settings: settings, commonFormat: .pcmFormatInt16, interleaved: true)
      let h = try FileHandle(forReadingFrom: input)
      defer { try? h.close() }
      while true {
        let d = h.readData(ofLength: 32768)
        if d.isEmpty { break }
        let n = d.count / 2
        guard let buf = AVAudioPCMBuffer(pcmFormat: fmt, frameCapacity: AVAudioFrameCount(n)) else { break }
        buf.frameLength = AVAudioFrameCount(n)
        d.withUnsafeBytes { raw in
          buf.int16ChannelData![0].update(from: raw.bindMemory(to: Int16.self).baseAddress!, count: n)
        }
        try file.write(from: buf)
      }
    }
    try? FileManager.default.removeItem(at: output)
    try FileManager.default.moveItem(at: tmp, to: output)
  }
}
