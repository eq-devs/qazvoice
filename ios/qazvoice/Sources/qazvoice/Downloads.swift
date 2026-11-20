import CommonCrypto
import Foundation

/// One download location of a file; headers go to this URL's origin only.
struct UrlSpec: Codable { let url: String; let headers: [String: String] }
struct FileSpec: Codable { let name: String; let size: Int64; let sha256: String; let urls: [UrlSpec] }

struct JobSpec: Codable {
  let id: String
  let version: Int
  let name: String
  let wifiOnly: Bool
  let files: [FileSpec]
  var totalBytes: Int64 { files.reduce(0) { $0 + $1.size } }

  static let idRe = try! NSRegularExpression(pattern: "^[a-z0-9][a-z0-9-]{0,40}$")
  static let nameRe = try! NSRegularExpression(pattern: "^[A-Za-z0-9_-][A-Za-z0-9._-]{0,100}$")
  static let shaRe = try! NSRegularExpression(pattern: "^[0-9a-f]{64}$")
  static let maxFile: Int64 = 4 << 30

  static func matches(_ re: NSRegularExpression, _ s: String) -> Bool {
    re.firstMatch(in: s, range: NSRange(s.startIndex..., in: s)) != nil
  }

  static func checkId(_ id: String?) throws -> String {
    guard let id = id, matches(idRe, id) else { throw DownloadError("argument", "bad model id") }
    return id
  }

  static func isSafeName(_ n: String) -> Bool {
    matches(nameRe, n) && !n.contains("..") && n != "installed.json" && n != "job.json"
      && !n.hasSuffix(".part") && !n.hasSuffix(".resume")
  }

  func validate() throws {
    _ = try JobSpec.checkId(id)
    guard !files.isEmpty, files.count <= 64 else { throw DownloadError("argument", "bad file list") }
    guard Set(files.map { $0.name }).count == files.count else { throw DownloadError("argument", "duplicate names") }
    for f in files {
      guard JobSpec.isSafeName(f.name), f.size >= 0, f.size <= JobSpec.maxFile,
            JobSpec.matches(JobSpec.shaRe, f.sha256), !f.urls.isEmpty else {
        throw DownloadError("argument", "bad file \(f.name)")
      }
      for u in f.urls {
        guard let s = URL(string: u.url)?.scheme?.lowercased(), s == "https" || s == "http" else {
          throw DownloadError("argument", "unsupported url")
        }
      }
    }
  }

  static func from(_ m: [String: Any]) throws -> JobSpec {
    guard let id = m["id"] as? String, let version = (m["version"] as? NSNumber)?.intValue,
          let files = m["files"] as? [[String: Any]] else { throw DownloadError("argument", "bad spec") }
    let fs: [FileSpec] = try files.map { f in
      guard let n = f["name"] as? String, let size = (f["size"] as? NSNumber)?.int64Value,
            let sha = f["sha256"] as? String, let urls = f["urls"] as? [[String: Any]] else {
        throw DownloadError("argument", "bad file")
      }
      return FileSpec(name: n, size: size, sha256: sha.lowercased(), urls: urls.map {
        UrlSpec(url: $0["url"] as? String ?? "", headers: ($0["headers"] as? [String: String]) ?? [:])
      })
    }
    return JobSpec(id: id, version: version, name: (m["name"] as? String) ?? id,
                   wifiOnly: (m["wifiOnly"] as? Bool) ?? false, files: fs)
  }
}

struct DownloadError: Error {
  let kind: String
  let message: String
  var extras: [String: Any] = [:]
  init(_ kind: String, _ message: String, _ extras: [String: Any] = [:]) {
    self.kind = kind; self.message = message; self.extras = extras
  }
  var map: [String: Any] { ["kind": kind, "message": message].merging(extras) { a, _ in a } }
}

/// Files on disk, same layout as Android:
/// `<root>/<id>/…` + `installed.json` (written last), `<root>/.staging/<id>/` for downloads.
final class ModelStore {
  let root: URL
  private let fm = FileManager.default

  init(root: URL) {
    self.root = root
    try? fm.createDirectory(at: root, withIntermediateDirectories: true)
    // Hundreds of MB that can be downloaded again: keep them out of iCloud backups.
    var r = root
    var v = URLResourceValues()
    v.isExcludedFromBackup = true
    try? r.setResourceValues(v)
  }

  func modelDir(_ id: String) -> URL { root.appendingPathComponent(id, isDirectory: true) }
  func stagingDir(_ id: String) -> URL {
    root.appendingPathComponent(".staging", isDirectory: true).appendingPathComponent(id, isDirectory: true)
  }

  func installed(_ id: String) -> (version: Int, bytes: Int64)? {
    let f = modelDir(id).appendingPathComponent("installed.json")
    guard let d = try? Data(contentsOf: f),
          let o = try? JSONSerialization.jsonObject(with: d) as? [String: Any],
          let v = (o["version"] as? NSNumber)?.intValue, let files = o["files"] as? [[String: Any]] else { return nil }
    var bytes: Int64 = 0
    for e in files {
      guard let n = e["name"] as? String, let s = (e["size"] as? NSNumber)?.int64Value else { return nil }
      if size(modelDir(id).appendingPathComponent(n)) != s { return nil }
      bytes += s
    }
    return (v, bytes)
  }

  func size(_ u: URL) -> Int64 {
    ((try? fm.attributesOfItem(atPath: u.path)[.size]) as? NSNumber)?.int64Value ?? -1
  }

  func install(_ job: JobSpec) throws {
    let staging = stagingDir(job.id), target = modelDir(job.id)
    let incoming = root.appendingPathComponent(".incoming-\(job.id)")
    let old = root.appendingPathComponent(".old-\(job.id)")
    try? fm.removeItem(at: incoming); try? fm.removeItem(at: old)
    try fm.createDirectory(at: incoming, withIntermediateDirectories: true)
    for f in job.files {
      let src = staging.appendingPathComponent(f.name)
      guard size(src) == f.size else { throw DownloadError("checksum", "cannot install \(f.name)", ["file": f.name]) }
      try fm.moveItem(at: src, to: incoming.appendingPathComponent(f.name))
    }
    if fm.fileExists(atPath: target.path) { try fm.moveItem(at: target, to: old) }
    do { try fm.moveItem(at: incoming, to: target) } catch {
      try? fm.moveItem(at: old, to: target); throw error
    }
    let info: [String: Any] = ["id": job.id, "version": job.version,
                               "installedAt": Int(Date().timeIntervalSince1970 * 1000),
                               "files": job.files.map { ["name": $0.name, "size": $0.size, "sha256": $0.sha256] }]
    let data = try JSONSerialization.data(withJSONObject: info)
    try data.write(to: target.appendingPathComponent("installed.json"), options: .atomic)
    try? fm.removeItem(at: old)
    try? fm.removeItem(at: staging)
  }

  func recover() {
    for u in (try? fm.contentsOfDirectory(at: root, includingPropertiesForKeys: nil)) ?? [] {
      let n = u.lastPathComponent
      if n.hasPrefix(".old-") {
        let target = modelDir(String(n.dropFirst(5)))
        if !fm.fileExists(atPath: target.appendingPathComponent("installed.json").path) {
          try? fm.removeItem(at: target); try? fm.moveItem(at: u, to: target)
        } else { try? fm.removeItem(at: u) }
      } else if n.hasPrefix(".incoming-") { try? fm.removeItem(at: u) }
    }
  }

  func delete(_ id: String) { try? fm.removeItem(at: modelDir(id)); try? fm.removeItem(at: stagingDir(id)) }
  func clearStaging(_ id: String) { try? fm.removeItem(at: stagingDir(id)) }

  struct Persisted: Codable { let spec: JobSpec; var status: String; var error: [String: String]? }

  func saveJob(_ spec: JobSpec, _ status: String, _ error: [String: Any]? = nil) {
    let dir = stagingDir(spec.id)
    try? fm.createDirectory(at: dir, withIntermediateDirectories: true)
    let p = Persisted(spec: spec, status: status, error: error?.mapValues { "\($0)" })
    if let d = try? JSONEncoder().encode(p) { try? d.write(to: dir.appendingPathComponent("job.json"), options: .atomic) }
  }

  func loadJob(_ id: String) -> Persisted? {
    guard let d = try? Data(contentsOf: stagingDir(id).appendingPathComponent("job.json")),
          let p = try? JSONDecoder().decode(Persisted.self, from: d), (try? p.spec.validate()) != nil else { return nil }
    return p
  }

  func persistedJobs() -> [Persisted] {
    let dir = root.appendingPathComponent(".staging")
    return ((try? fm.contentsOfDirectory(atPath: dir.path)) ?? []).compactMap { loadJob($0) }
  }

  func usedBytes() -> Int64 {
    var total: Int64 = 0
    if let e = fm.enumerator(at: root, includingPropertiesForKeys: [.fileSizeKey]) {
      for case let u as URL in e { total += Int64((try? u.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0) }
    }
    return total
  }

  func freeBytes() -> Int64 {
    let v = try? root.resourceValues(forKeys: [.volumeAvailableCapacityForImportantUsageKey])
    return v?.volumeAvailableCapacityForImportantUsage ?? -1
  }
}

func sha256Hex(of url: URL) -> String? {
  guard let h = try? FileHandle(forReadingFrom: url) else { return nil }
  defer { try? h.close() }
  var ctx = CC_SHA256_CTX()
  CC_SHA256_Init(&ctx)
  while true {
    let d = h.readData(ofLength: 1 << 20)
    if d.isEmpty { break }
    d.withUnsafeBytes { _ = CC_SHA256_Update(&ctx, $0.baseAddress, CC_LONG(d.count)) }
  }
  var out = [UInt8](repeating: 0, count: Int(CC_SHA256_DIGEST_LENGTH))
  CC_SHA256_Final(&out, &ctx)
  return out.map { String(format: "%02x", $0) }.joined()
}

/// Model downloads on iOS: a background URLSession (continues while the app is
/// suspended or terminated by the system), one file at a time, verified with
/// SHA-256 before install; resume data is kept on pause and errors; retries
/// with backoff and mirror failover. Mirrors with headers use a foreground
/// session so the headers never follow a redirect to another origin.
final class DownloadManager: NSObject, URLSessionDownloadDelegate {
  static let shared = DownloadManager()

  private let q = DispatchQueue(label: "kz.qazvoice.downloads")
  private(set) var store: ModelStore!
  private var background: URLSession!
  private var foreground: URLSession!
  var backgroundCompletion: (() -> Void)?
  var listener: (([String: Any]) -> Void)?
  private var initialized = false

  private final class Job {
    let spec: JobSpec
    var status = "queued"
    var error: [String: Any]?
    var fileIndex = 0
    var mirror = 0
    var attempt = 0
    var checksumFailures = 0
    var task: URLSessionDownloadTask?
    var currentWritten: Int64 = 0
    var lastEmit = Date.distantPast
    var speed = 0.0
    var windowStart = Date()
    var windowBytes: Int64 = 0
    var stopping: String?  // "pause" | "cancel"
    init(_ s: JobSpec) { spec = s }
  }

  private var jobs: [String: Job] = [:]
  private var order: [String] = []
  private var running: String?

  func initialize() {
    q.sync {
      if initialized { return }
      initialized = true
      let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
      store = ModelStore(root: support.appendingPathComponent("qazvoice/models", isDirectory: true))
      store.recover()
      let id = "kz.qazvoice.downloads.\(Bundle.main.bundleIdentifier ?? "app")"
      let bg = URLSessionConfiguration.background(withIdentifier: id)
      bg.sessionSendsLaunchEvents = true
      bg.isDiscretionary = false
      background = URLSession(configuration: bg, delegate: self, delegateQueue: nil)
      let fg = URLSessionConfiguration.default
      fg.waitsForConnectivity = true
      fg.timeoutIntervalForResource = 60 * 60 * 6
      foreground = URLSession(configuration: fg, delegate: self, delegateQueue: nil)
      for p in store.persistedJobs() {
        let j = Job(p.spec)
        j.status = (p.status == "paused" || p.status == "failed") ? p.status : "queued"
        if let e = p.error { j.error = e }
        jobs[p.spec.id] = j
        order.append(p.spec.id)
      }
    }
    // Re-attach to transfers the system kept running while the app was gone.
    background.getAllTasks { tasks in
      self.q.async {
        for t in tasks {
          guard let d = t as? URLSessionDownloadTask, let desc = d.taskDescription,
                let id = desc.split(separator: "|").first.map(String.init), let j = self.jobs[id] else {
            t.cancel(); continue
          }
          j.task = d; j.status = "downloading"; self.running = id
          if let i = j.spec.files.firstIndex(where: { $0.name == desc.split(separator: "|").dropFirst().first.map(String.init) }) {
            j.fileIndex = i
          }
        }
        self.pump()
      }
    }
  }

  // MARK: - API (any thread)

  func state(_ id: String) -> [String: Any] { q.sync { stateLocked(id) } }

  private func stateLocked(_ id: String) -> [String: Any] {
    var base: [String: Any] = [:]
    let inst = store.installed(id)
    if let i = inst { base["installedVersion"] = i.version; base["installedBytes"] = i.bytes }
    guard let j = jobs[id] else {
      if let i = inst { return base.merging(["status": "installed", "version": i.version]) { a, _ in a } }
      return base.merging(["status": "notInstalled", "received": 0]) { a, _ in a }
    }
    let rec = received(j)
    let total = j.spec.totalBytes
    let s: [String: Any]
    switch j.status {
    case "downloading": s = ["status": "downloading", "received": rec, "total": total, "speed": j.speed]
    case "queued": s = ["status": "waiting", "reason": "queued", "received": rec, "total": total]
    case "failed": s = ["status": "failed", "received": rec, "total": total, "error": j.error ?? [:]]
    default: s = ["status": "paused", "received": rec, "total": total]
    }
    return base.merging(s) { _, b in b }
  }

  func states() -> [String: Any] {
    q.sync {
      var ids = Set(jobs.keys)
      for n in (try? FileManager.default.contentsOfDirectory(atPath: store.root.path)) ?? [] where !n.hasPrefix(".") {
        ids.insert(n)
      }
      var out: [String: Any] = [:]
      for id in ids where JobSpec.matches(JobSpec.idRe, id) { out[id] = stateLocked(id) }
      return out
    }
  }

  func enqueue(_ spec: JobSpec) throws {
    try spec.validate()
    q.async {
      if let e = self.jobs[spec.id], e.status == "queued" || e.status == "downloading" { return }
      if let prev = self.store.loadJob(spec.id), prev.spec.version != spec.version { self.store.clearStaging(spec.id) }
      self.jobs[spec.id] = Job(spec)
      self.order.removeAll { $0 == spec.id }
      self.order.append(spec.id)
      self.store.saveJob(spec, "queued")
      self.emit(spec.id, self.stateLocked(spec.id).filter { $0.key != "installedVersion" && $0.key != "installedBytes" })
      self.pump()
    }
  }

  func pause(_ id: String) {
    q.sync {
      guard let j = self.jobs[id] else { return }
      if j.status == "queued" { self.markPaused(j); return }
      guard j.status == "downloading", let t = j.task else { return }
      j.stopping = "pause"
      t.cancel { data in
        self.q.async {
          if let d = data { try? d.write(to: self.resumeURL(j)) }
          self.markPaused(j)
          self.running = nil
          self.pump()
        }
      }
    }
  }

  func resume(_ id: String) {
    q.sync {
      let spec = self.jobs[id]?.spec ?? self.store.loadJob(id)?.spec
      guard let s = spec else { return }
      if let j = self.jobs[id], j.status == "queued" || j.status == "downloading" { return }
      self.jobs[id] = nil
      self.q.async { try? self.enqueue(s) }
    }
  }

  func cancel(_ id: String) {
    q.sync {
      guard let j = self.jobs[id] else { self.store.clearStaging(id); return }
      j.stopping = "cancel"
      j.task?.cancel()
      self.finishCancel(j)
    }
  }

  func delete(_ id: String) {
    q.sync {
      if let j = self.jobs[id] { j.stopping = "cancel"; j.task?.cancel() }
      self.jobs[id] = nil
      if self.running == id { self.running = nil }
      self.store.delete(id)
      self.emit(id, ["status": "notInstalled", "received": 0])
      self.pump()
    }
  }

  // MARK: - internals (on q)

  private func emit(_ id: String, _ m: [String: Any]) {
    var e = m
    e["type"] = "download"; e["id"] = id
    listener?(e)
  }

  private func log(_ s: String) { listener?(["type": "log", "level": "info", "message": s]) }

  private func resumeURL(_ j: Job) -> URL {
    store.stagingDir(j.spec.id).appendingPathComponent("\(j.spec.files[j.fileIndex].name).resume")
  }

  private func verifiedBytes(_ j: Job) -> Int64 {
    j.spec.files.filter { store.size(store.stagingDir(j.spec.id).appendingPathComponent($0.name)) == $0.size }
      .reduce(0) { $0 + $1.size }
  }

  private func received(_ j: Job) -> Int64 { min(j.spec.totalBytes, verifiedBytes(j) + (j.status == "downloading" ? j.currentWritten : 0)) }

  private func markPaused(_ j: Job) {
    j.status = "paused"; j.task = nil; j.stopping = nil
    store.saveJob(j.spec, "paused")
    emit(j.spec.id, stateLocked(j.spec.id))
  }

  private func finishCancel(_ j: Job) {
    if jobs[j.spec.id] === j { jobs[j.spec.id] = nil }
    if running == j.spec.id { running = nil }
    store.clearStaging(j.spec.id)
    emit(j.spec.id, ["status": "notInstalled", "received": 0, "keepInstalled": store.installed(j.spec.id) != nil])
    pump()
  }

  private func fail(_ j: Job, _ e: DownloadError) {
    j.status = "failed"; j.error = e.map; j.task = nil
    if running == j.spec.id { running = nil }
    store.saveJob(j.spec, "failed", e.map)
    log("\(j.spec.id): failed: \(e.message)")
    emit(j.spec.id, ["status": "failed", "received": received(j), "total": j.spec.totalBytes, "error": e.map])
    pump()
  }

  private func pump() {
    if let r = running, jobs[r]?.status == "downloading" { return }
    running = nil
    guard let id = order.first(where: { jobs[$0]?.status == "queued" }), let j = jobs[id] else { return }
    running = id
    j.status = "downloading"
    store.saveJob(j.spec, "downloading")
    for other in order where other != id && jobs[other]?.status == "queued" {
      emit(other, stateLocked(other))
    }
    let need = j.spec.totalBytes - verifiedBytes(j) + (16 << 20)
    let free = store.freeBytes()
    if free >= 0 && free < need {
      fail(j, DownloadError("storage", "not enough free storage", ["needed": need, "available": free])); return
    }
    log("\(id): downloading v\(j.spec.version) (\(j.spec.totalBytes) bytes)")
    nextFile(j)
  }

  private func nextFile(_ j: Job) {
    let staging = store.stagingDir(j.spec.id)
    try? FileManager.default.createDirectory(at: staging, withIntermediateDirectories: true)
    guard let i = j.spec.files.firstIndex(where: { store.size(staging.appendingPathComponent($0.name)) != $0.size }) else {
      install(j); return
    }
    if i != j.fileIndex { j.mirror = 0; j.attempt = 0; j.checksumFailures = 0 }
    j.fileIndex = i
    j.currentWritten = 0
    let f = j.spec.files[i]
    let u = f.urls[j.mirror]
    guard let url = URL(string: u.url) else { fail(j, DownloadError("argument", "bad url")); return }
    var req = URLRequest(url: url)
    req.allowsCellularAccess = !j.spec.wifiOnly
    req.setValue("identity", forHTTPHeaderField: "Accept-Encoding")
    for (k, v) in u.headers { req.setValue(v, forHTTPHeaderField: k) }
    let session = u.headers.isEmpty ? background! : foreground!
    let task: URLSessionDownloadTask
    if let d = try? Data(contentsOf: resumeURL(j)), u.headers.isEmpty {
      task = session.downloadTask(withResumeData: d)
    } else {
      task = session.downloadTask(with: req)
    }
    try? FileManager.default.removeItem(at: resumeURL(j))
    task.taskDescription = "\(j.spec.id)|\(f.name)"
    j.task = task
    task.resume()
  }

  private func install(_ j: Job) {
    emit(j.spec.id, ["status": "verifying", "received": j.spec.totalBytes, "total": j.spec.totalBytes])
    do {
      guard jobs[j.spec.id] === j, j.stopping == nil else { return }
      try store.install(j.spec)
      jobs[j.spec.id] = nil
      running = nil
      let bytes = store.installed(j.spec.id)?.bytes ?? j.spec.totalBytes
      log("\(j.spec.id): installed v\(j.spec.version)")
      emit(j.spec.id, ["status": "installed", "version": j.spec.version, "installedBytes": bytes])
      pump()
    } catch let e as DownloadError { fail(j, e) } catch { fail(j, DownloadError("storage", "\(error)")) }
  }

  private func job(for task: URLSessionTask) -> Job? {
    guard let id = task.taskDescription?.split(separator: "|").first.map(String.init), let j = jobs[id], j.task === task
    else { return nil }
    return j
  }

  /// Retry with backoff; after `maxAttempts` (or a fatal status) try the next mirror.
  private func retry(_ j: Job, _ why: String, fatalForMirror: Bool, status: Int? = nil) {
    let f = j.spec.files[j.fileIndex]
    j.attempt += 1
    if fatalForMirror || j.attempt >= 5 {
      j.mirror += 1; j.attempt = 0
      if j.mirror >= f.urls.count {
        var extras: [String: Any] = [:]
        if let s = status { extras["status"] = s }
        fail(j, DownloadError("network", why, extras)); return
      }
      log("\(j.spec.id): \(f.name): \(why), next mirror")
      nextFile(j); return
    }
    let delay = min(30.0, pow(2.0, Double(j.attempt - 1))) * (1 + Double.random(in: 0..<0.2))
    log("\(j.spec.id): \(f.name): \(why); retry \(j.attempt) in \(Int(delay * 1000)) ms")
    q.asyncAfter(deadline: .now() + delay) { [weak self] in
      guard let self = self, self.jobs[j.spec.id] === j, j.status == "downloading" else { return }
      self.nextFile(j)
    }
  }

  // MARK: - URLSession delegate

  func urlSession(_ session: URLSession, downloadTask: URLSessionDownloadTask, didWriteData bytesWritten: Int64,
                  totalBytesWritten: Int64, totalBytesExpectedToWrite: Int64) {
    q.async {
      guard let j = self.job(for: downloadTask) else { return }
      j.currentWritten = totalBytesWritten
      j.windowBytes += bytesWritten  // not the total: a re-attached task starts mid-file
      let now = Date()
      let dt = now.timeIntervalSince(j.windowStart)
      if dt >= 1 {
        let inst = Double(j.windowBytes) / dt
        j.speed = j.speed == 0 ? inst : j.speed * 0.6 + inst * 0.4
        j.windowStart = now; j.windowBytes = 0
      }
      if now.timeIntervalSince(j.lastEmit) >= 0.2 {
        j.lastEmit = now
        self.emit(j.spec.id, ["status": "downloading", "received": self.received(j), "total": j.spec.totalBytes, "speed": j.speed])
      }
    }
  }

  func urlSession(_ session: URLSession, downloadTask: URLSessionDownloadTask, didFinishDownloadingTo location: URL) {
    // The temporary file is deleted when this returns: move it now.
    let desc = downloadTask.taskDescription ?? ""
    let parts = desc.split(separator: "|").map(String.init)
    guard parts.count == 2, (try? JobSpec.checkId(parts[0])) != nil, JobSpec.isSafeName(parts[1]),
          let staging = q.sync(execute: { store?.stagingDir(parts[0]) }) else { return }
    let tmp = staging.appendingPathComponent("\(parts[1]).part")
    try? FileManager.default.createDirectory(at: staging, withIntermediateDirectories: true)
    try? FileManager.default.removeItem(at: tmp)
    let status = (downloadTask.response as? HTTPURLResponse)?.statusCode ?? 0
    if (200..<300).contains(status) { try? FileManager.default.moveItem(at: location, to: tmp) }
    q.async {
      guard let j = self.job(for: downloadTask) else { try? FileManager.default.removeItem(at: tmp); return }
      j.task = nil
      if !(200..<300).contains(status) {
        self.retry(j, "HTTP \(status)", fatalForMirror: [400, 401, 403, 404, 410, 451].contains(status), status: status)
        return
      }
      let f = j.spec.files[j.fileIndex]
      // Hash off the session queue; this is the expensive part.
      DispatchQueue.global(qos: .utility).async {
        let ok = self.store.size(tmp) == f.size && sha256Hex(of: tmp) == f.sha256
        self.q.async {
          guard self.jobs[j.spec.id] === j, j.stopping == nil else { try? FileManager.default.removeItem(at: tmp); return }
          if ok {
            try? FileManager.default.moveItem(at: tmp, to: staging.appendingPathComponent(f.name))
            j.attempt = 0; j.checksumFailures = 0
            self.nextFile(j)
          } else {
            try? FileManager.default.removeItem(at: tmp)
            j.checksumFailures += 1
            if j.checksumFailures >= 2 {
              j.checksumFailures = 0
              j.mirror += 1; j.attempt = 0
              if j.mirror >= f.urls.count {
                self.fail(j, DownloadError("checksum", "\(f.name): SHA-256 mismatch", ["file": f.name])); return
              }
            }
            self.nextFile(j)
          }
        }
      }
    }
  }

  func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
    guard let error = error else { return }
    q.async {
      guard let j = self.job(for: task), j.stopping == nil else { return }
      j.task = nil
      let ns = error as NSError
      if let d = ns.userInfo[NSURLSessionDownloadTaskResumeData] as? Data { try? d.write(to: self.resumeURL(j)) }
      if ns.code == NSURLErrorCancelled { return }
      if let posix = (ns.userInfo[NSUnderlyingErrorKey] as? NSError), posix.domain == NSPOSIXErrorDomain, posix.code == Int(ENOSPC) {
        self.fail(j, DownloadError("storage", "storage full", ["needed": j.spec.files[j.fileIndex].size, "available": self.store.freeBytes()]))
        return
      }
      self.retry(j, ns.localizedDescription, fatalForMirror: false)
    }
  }

  func urlSession(_ session: URLSession, taskIsWaitingForConnectivity task: URLSessionTask) {
    q.async {
      guard let j = self.job(for: task) else { return }
      self.emit(j.spec.id, ["status": "waiting", "reason": j.spec.wifiOnly ? "wifi" : "network",
                            "received": self.received(j), "total": j.spec.totalBytes])
    }
  }

  /// Foreground session only: drop the mirror's headers when a redirect leaves its origin.
  func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
                  newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) {
    func origin(_ u: URL?) -> String { "\(u?.scheme ?? "")://\(u?.host ?? ""):\(u?.port ?? -1)" }
    guard let from = task.originalRequest?.url else { completionHandler(request); return }
    if request.url?.scheme == "http" && from.scheme == "https" { completionHandler(nil); return }
    if origin(from) == origin(request.url) { completionHandler(request); return }
    var r = request
    for k in task.originalRequest?.allHTTPHeaderFields?.keys ?? [:].keys where k.lowercased() != "accept-encoding" {
      r.setValue(nil, forHTTPHeaderField: k)
    }
    completionHandler(r)
  }

  func urlSessionDidFinishEvents(forBackgroundURLSession session: URLSession) {
    DispatchQueue.main.async {
      self.backgroundCompletion?()
      self.backgroundCompletion = nil
    }
  }

  var isBusy: Bool { q.sync { jobs.values.contains { $0.status == "queued" || $0.status == "downloading" } } }
  func usedBytes() -> Int64 { store.usedBytes() }
}
