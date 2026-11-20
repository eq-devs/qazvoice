import 'dart:io';
import 'dart:math' as math;
import 'dart:typed_data';

/// 16-bit mono PCM → a WAV file in memory.
Uint8List pcm16ToWav(Int16List pcm, int sampleRate, {int channels = 1}) {
  final dataLen = pcm.lengthInBytes;
  final b = ByteData(44 + dataLen);
  void str(int o, String s) {
    for (var i = 0; i < s.length; i++) {
      b.setUint8(o + i, s.codeUnitAt(i));
    }
  }

  str(0, 'RIFF');
  b.setUint32(4, 36 + dataLen, Endian.little);
  str(8, 'WAVE');
  str(12, 'fmt ');
  b.setUint32(16, 16, Endian.little);
  b.setUint16(20, 1, Endian.little);
  b.setUint16(22, channels, Endian.little);
  b.setUint32(24, sampleRate, Endian.little);
  b.setUint32(28, sampleRate * channels * 2, Endian.little);
  b.setUint16(32, channels * 2, Endian.little);
  b.setUint16(34, 16, Endian.little);
  str(36, 'data');
  b.setUint32(40, dataLen, Endian.little);
  final out = b.buffer.asUint8List();
  out.setRange(
      44, 44 + dataLen, pcm.buffer.asUint8List(pcm.offsetInBytes, dataLen));
  return out;
}

/// Loudness of [pcm] for meters: RMS mapped from −60…0 dBFS to 0…1.
double pcmLevel(Int16List pcm) {
  if (pcm.isEmpty) return 0;
  var sum = 0.0;
  final step = pcm.length > 4000 ? 2 : 1;
  var n = 0;
  for (var i = 0; i < pcm.length; i += step) {
    final v = pcm[i] / 32768.0;
    sum += v * v;
    n++;
  }
  final rms = math.sqrt(sum / n);
  if (rms <= 1e-6) return 0;
  final db = 20 * math.log(rms) / math.ln10;
  return ((db + 60) / 60).clamp(0.0, 1.0);
}

/// Bytes of a little-endian 16-bit buffer as samples (copies if unaligned).
Int16List bytesToPcm16(Uint8List bytes) {
  final even = bytes.lengthInBytes & ~1;
  if (bytes.offsetInBytes.isEven) {
    return bytes.buffer.asInt16List(bytes.offsetInBytes, even ~/ 2);
  }
  return Uint8List.fromList(bytes.sublist(0, even)).buffer.asInt16List();
}

/// Samples as bytes (no copy).
Uint8List pcm16ToBytes(Int16List pcm) =>
    pcm.buffer.asUint8List(pcm.offsetInBytes, pcm.lengthInBytes);

/// The PCM part of a WAV file.
class WavInfo {
  const WavInfo(
      this.sampleRate, this.channels, this.dataOffset, this.dataLength);
  final int sampleRate, channels, dataOffset, dataLength;
}

/// Parses a 16-bit PCM WAV header; null if [file] is anything else
/// (compressed WAV, float, not RIFF).
Future<WavInfo?> readWavInfo(File file) async {
  final raf = await file.open();
  try {
    final len = await raf.length();
    final head = await raf.read(math.min(len, 1 << 16));
    if (head.length < 44) return null;
    final b = ByteData.sublistView(head);
    String tag(int o) => String.fromCharCodes(head.sublist(o, o + 4));
    if (tag(0) != 'RIFF' || tag(8) != 'WAVE') return null;
    var o = 12;
    int? rate, channels, bits, format;
    while (o + 8 <= head.length) {
      final id = tag(o);
      final size = b.getUint32(o + 4, Endian.little);
      if (id == 'fmt ' && o + 24 <= head.length) {
        format = b.getUint16(o + 8, Endian.little);
        channels = b.getUint16(o + 10, Endian.little);
        rate = b.getUint32(o + 12, Endian.little);
        bits = b.getUint16(o + 22, Endian.little);
        if (format == 0xFFFE && o + 34 <= head.length) {
          format = b.getUint16(o + 32, Endian.little); // WAVE_FORMAT_EXTENSIBLE
        }
      } else if (id == 'data') {
        if (format != 1 ||
            bits != 16 ||
            rate == null ||
            channels == null ||
            channels == 0) {
          return null;
        }
        final start = o + 8;
        final dataLen = (size == 0 || start + size > len) ? len - start : size;
        return WavInfo(rate, channels, start, dataLen & ~1);
      }
      o += 8 + size + (size & 1);
    }
    return null;
  } finally {
    await raf.close();
  }
}
