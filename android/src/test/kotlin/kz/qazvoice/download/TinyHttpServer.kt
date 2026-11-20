package kz.qazvoice.download

import java.io.BufferedInputStream
import java.io.OutputStream
import java.net.InetAddress
import java.net.ServerSocket
import java.net.Socket
import kotlin.concurrent.thread

/** Minimal HTTP/1.1 server for tests (one request per connection). */
class TinyHttpServer(private val handler: (Exchange) -> Unit) {
    class Exchange(val method: String, val path: String, val headers: Map<String, String>, private val socket: Socket) {
        val out: OutputStream = socket.getOutputStream()
        private var sent = false
        fun header(name: String): String? = headers[name.lowercase()]

        fun respond(code: Int, headers: Map<String, String> = emptyMap(), length: Long = -1) {
            check(!sent); sent = true
            val sb = StringBuilder("HTTP/1.1 $code X\r\n")
            headers.forEach { (k, v) -> sb.append("$k: $v\r\n") }
            if (length >= 0) sb.append("Content-Length: $length\r\n")
            sb.append("Connection: close\r\n\r\n")
            out.write(sb.toString().toByteArray())
            out.flush()
        }

        fun close() = runCatching { socket.close() }
    }

    private val server = ServerSocket(0, 50, InetAddress.getByName("127.0.0.1"))
    val port: Int get() = server.localPort
    @Volatile private var running = true

    init {
        thread(isDaemon = true) {
            while (running) {
                val s = runCatching { server.accept() }.getOrNull() ?: break
                thread(isDaemon = true) { serve(s) }
            }
        }
    }

    private fun serve(s: Socket) {
        try {
            val input = BufferedInputStream(s.getInputStream())
            fun line(): String {
                val sb = StringBuilder()
                while (true) {
                    val c = input.read()
                    if (c < 0 || c == '\n'.code) break
                    if (c != '\r'.code) sb.append(c.toChar())
                }
                return sb.toString()
            }
            val req = line().split(' ')
            if (req.size < 2) return
            val headers = HashMap<String, String>()
            while (true) {
                val l = line()
                if (l.isEmpty()) break
                val i = l.indexOf(':')
                if (i > 0) headers[l.substring(0, i).trim().lowercase()] = l.substring(i + 1).trim()
            }
            handler(Exchange(req[0], req[1].substringBefore('?'), headers, s))
        } catch (_: Exception) {
        } finally {
            runCatching { s.close() }
        }
    }

    fun stop() {
        running = false
        server.close()
    }
}
