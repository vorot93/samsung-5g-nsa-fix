package me.vorotnikov.nsa5gfix

import android.content.Context
import java.io.BufferedReader
import java.io.InputStreamReader

/** Single owner of the root-side nsa5gfix-diagd child (nsg-unlock CliHost style):
 *  the bundled jniLibs daemon (libnsa5gfix-diagd.so) is staged to
 *  /data/local/tmp and exec'd through su, so the DIAG reader runs as a
 *  child of this app's su wrapper — its lifetime is the tray session.
 *  Every output line lands in a bounded ring buffer broadcast to
 *  listeners; the daemon publishes its state to dev.nsa5gfix.* system
 *  properties (arfcn/pci/band/ts + its own pid), which the tray card and
 *  the Frida hook in com.android.phone both consume.
 *
 *  Stop is a VERIFIED root-side kill: the daemon's pid is read back from
 *  dev.nsa5gfix.pid (written by the daemon itself), killed via su, and the
 *  su wrapper is destroyed as the suspenders. A daemon exit on its own
 *  ends the session too — the tray follows the child, never the reverse. */
object DaemonHost {
    private const val LOG_CAP = 500
    private const val STAGED = "/data/local/tmp/nsa5gfix-diagd"

    interface Listener {
        fun onLog(line: String)
        fun onChild(alive: Boolean, detail: String)
    }

    private val lock = Any()
    private var process: Process? = null
    private val log = ArrayDeque<String>()
    private val listeners = mutableSetOf<Listener>()

    val childAlive: Boolean
        get() = synchronized(lock) { process?.isAlive == true }

    fun addListener(listener: Listener) {
        val snapshot: List<String>
        val alive: Boolean
        synchronized(lock) {
            listeners.add(listener)
            snapshot = log.toList()
            alive = childAlive
        }
        snapshot.forEach(listener::onLog)
        listener.onChild(alive, if (alive) "running" else "not running")
    }

    fun removeListener(listener: Listener) {
        synchronized(lock) { listeners.remove(listener) }
    }

    fun snapshot(): List<String> = synchronized(lock) { log.toList() }

    private fun append(line: String) {
        val listeners: Set<Listener>
        synchronized(lock) {
            if (log.size >= LOG_CAP) log.removeFirst()
            log.addLast(line)
            listeners = this.listeners.toSet()
        }
        listeners.forEach { it.onLog(line) }
    }

    private fun broadcastChild(alive: Boolean, detail: String) {
        val listeners: Set<Listener>
        synchronized(lock) { listeners = this.listeners.toSet() }
        listeners.forEach { it.onChild(alive, detail) }
    }

    /** Stages the bundled daemon and execs it as root. The su prompt may
     *  block on a first-run KernelSU consent dialog — all of this runs off
     *  the main thread, and a second start while a child lives is refused. */
    fun start(context: Context) {
        val lib = context.applicationInfo.nativeLibraryDir + "/libnsa5gfix-diagd.so"
        synchronized(lock) {
            if (childAlive) {
                append("nsa5gfix-diagd already running; tap Stop on the card first")
                return
            }
        }
        val command = "cp '$lib' $STAGED && chmod 755 $STAGED && exec $STAGED --verbose"
        Thread({
            append("")
            append("$ su -c $STAGED --verbose")
            try {
                val proc = Runtime.getRuntime().exec(arrayOf("su", "-c", command))
                synchronized(lock) { process = proc }
                broadcastChild(true, "running")
                val stdout = BufferedReader(InputStreamReader(proc.inputStream))
                val stderr = BufferedReader(InputStreamReader(proc.errorStream))
                Thread({
                    try { stderr.forEachLine(::append) } catch (_: Exception) {}
                }, "nsa5gfix-diagd-err").start()
                try {
                    stdout.forEachLine(::append)
                } catch (_: Exception) {}
                val rc = runCatching { proc.waitFor() }.getOrDefault(-1)
                synchronized(lock) { process = null }
                append("nsa5gfix-diagd exited (rc=$rc)")
                broadcastChild(false, "exited rc=$rc")
            } catch (e: Exception) {
                append("failed to start su child: $e")
                broadcastChild(false, "su failed: ${e.message}")
            }
        }, "nsa5gfix-diagd-host").start()
    }

    /** Root-side kill of exactly the daemon (belt: dev.nsa5gfix.pid read
     *  back from the property the daemon wrote; suspenders: pkill by the
     *  staged path, then the su wrapper destroy). Fire-and-forget — the
     *  child's own exit delivers the session-end broadcast. */
    fun stop() {
        Thread({
            try {
                Runtime.getRuntime().exec(
                    arrayOf(
                        "su", "-c",
                        "kill \$(getprop dev.nsa5gfix.pid) 2>/dev/null; " +
                            "pkill -f '$STAGED' 2>/dev/null; true",
                    ),
                ).waitFor()
            } catch (_: Exception) {}
            synchronized(lock) { process?.destroyForcibly() }
        }, "nsa5gfix-diagd-stop").start()
    }

    /** One dev.nsa5gfix.* property read (SystemProperties is @hide). */
    fun prop(name: String): String = try {
        Class.forName("android.os.SystemProperties")
            .getMethod("get", String::class.java, String::class.java)
            .invoke(null, name, "") as String
    } catch (_: Exception) {
        ""
    }
}
