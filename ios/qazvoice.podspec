Pod::Spec.new do |s|
  s.name             = 'qazvoice'
  s.version          = '0.0.2'
  s.summary          = 'On-device Kazakh text-to-speech and speech recognition.'
  s.description      = <<-DESC
On-device Kazakh text-to-speech and speech recognition for Flutter, with
background model downloads, playback, microphone capture and file decoding.
                       DESC
  s.homepage         = 'https://github.com/eq-devs/qazvoice'
  s.license          = { :file => '../LICENSE' }
  s.author           = { 'eq-devs' => 'https://github.com/eq-devs' }
  s.source           = { :path => '.' }
  s.source_files     = 'qazvoice/Sources/qazvoice/**/*.swift'
  s.resource_bundles = { 'qazvoice_privacy' => ['qazvoice/Sources/qazvoice/PrivacyInfo.xcprivacy'] }
  # libqazvoice (Rust engine + ONNX Runtime) as a dynamic framework, loaded by dart:ffi.
  s.vendored_frameworks = 'qazvoice/Frameworks/QazVoiceNative.xcframework'
  s.frameworks       = 'AVFoundation', 'CoreML'
  s.dependency 'Flutter'
  s.platform = :ios, '15.1'
  s.pod_target_xcconfig = { 'DEFINES_MODULE' => 'YES', 'EXCLUDED_ARCHS[sdk=iphonesimulator*]' => 'i386 x86_64' }
  s.swift_version = '5.0'
end
