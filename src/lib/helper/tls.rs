use gio::prelude::*;
use tracing::*;

/// Construct the default GIO TLS backend at startup. This makes sure
/// GnuTLS is properly initialized before WebRTC's STUN code uses it.
///
/// # Why this is needed
///
/// We have only seen this crash in one deployment so far. The reference
/// setup, with the same Raspberry Pi model, the same Raspbian release,
/// the same BlueOS version, the same dependency set, and the same
/// WebRTC client, on a quiet isolated subnet, does not reproduce the
/// crash. The affected setup is on a busy LAN shared with several
/// other devices. Our best guess is that the bug is present on every
/// instance of this software stack, but only produces a clear SIGSEGV
/// when memory happens to be in a specific state at the moment WebRTC
/// asks for its first HMAC. Other memory states can instead cause a
/// silent ICE authentication failure, or a different crash that is
/// harder to recognize. The state of memory at that point depends on
/// which allocations happened before the WebRTC connection, and that
/// depends on background network traffic. The call below is cheap and
/// avoids all of these failure modes.
///
/// # Problem in short
///
/// `libgnutls` is left uninitialized when `libnice` asks it to compute
/// the HMAC over a STUN message. `gnutls_hmac_init()` returns an error
/// that `libnice` does not check, and the next call to `gnutls_hmac()`
/// reads uninitialized memory as a function pointer and follows it,
/// which crashes the process.
///
/// # Problem in detail
///
/// Three libraries are involved:
///
/// - `libgnutls` is a TLS and cryptography library. It implements many
///   things, including HMAC, which is used to authenticate STUN messages.
/// - `libnice` is the ICE and STUN library used internally by
///   GStreamer's `webrtcbin`. It calls `libgnutls` directly to compute
///   the HMAC over each outgoing STUN message.
/// - `libgiognutls.so` is an optional GIO module shipped by
///   `glib-networking`. When it is present, programs that use GLib's
///   high-level I/O API can open TLS connections, and `libgiognutls.so`
///   uses `libgnutls` underneath to do the actual TLS work.
///
/// `libgnutls` requires its global `gnutls_global_init()` function to be
/// called once before any other `libgnutls` function is safely usable.
/// Normally `libgnutls` does this on its own. It contains an ELF
/// constructor function that the dynamic linker runs as soon as
/// `libgnutls` is loaded into the process, and that constructor calls
/// `gnutls_global_init()`.
///
/// Before calling `gnutls_global_init()`, the constructor checks a
/// *weak* symbol named `_gnutls_global_init_skip`. `libgnutls` defines
/// this weak symbol as a function that returns `0`, meaning "do not
/// skip". When `libgiognutls.so` is also in the process, however, it
/// defines a *strong* version of the same symbol that returns `1`,
/// meaning "skip". The dynamic linker chooses the strong version, so
/// the `libgnutls` constructor sees `1` and skips initialization. The
/// library is loaded but not initialized.
///
/// The agreement between these two libraries is that `libgiognutls.so`
/// will call `gnutls_global_init()` itself, but only the first time GIO
/// is asked for a TLS connection. The override exists so that both
/// libraries do not initialize GnuTLS twice in programs that use only
/// one of them.
///
/// MCM's TLS path goes through `webrtcbin`, then `libnice`, then
/// `libgnutls`. It never asks GIO for a TLS connection. So this is
/// what happens on the affected setup when a peer connects:
///
/// 1. GStreamer (or another GLib subsystem) loads `libgiognutls.so`.
/// 2. `libgnutls` is loaded as a dependency of `libgiognutls.so`. The
///    `libgnutls` constructor runs, sees the override returning `1`,
///    and skips initialization. The internal `libgnutls` library state
///    stays at `LIB_STATE_POWERON`, which means "not yet initialized".
/// 3. Nothing ever asks GIO for TLS, so `libgiognutls.so` never calls
///    `gnutls_global_init()` either.
/// 4. `webrtcbin` starts ICE checks. `libnice` builds an outgoing STUN
///    message and calls `gnutls_hmac_init()` to compute its HMAC.
/// 5. `gnutls_hmac_init()` allocates a buffer for the HMAC state using
///    `gnutls_malloc()`. `gnutls_malloc()` is plain `malloc`, so the
///    buffer contains whatever bytes were already in that piece of
///    memory. Then `gnutls_hmac_init()` calls an internal function
///    `_gnutls_mac_init()` to fill the buffer with function pointers
///    and algorithm context.
/// 6. `_gnutls_mac_init()` first checks the library state. The state
///    is `LIB_STATE_POWERON`, so the function returns a negative error
///    code *without writing anything to the buffer*. The buffer keeps
///    its old contents. In the case we observed, those old contents
///    were leftover strings from the GStreamer plugin registry, for
///    example "Proxy Sink".
/// 7. `gnutls_hmac_init()` returns the same error code, but `libnice`'s
///    `stun_agent_finish_message()` does not check the return value.
///    It uses the buffer as if it were a valid HMAC handle.
/// 8. `libnice` calls `gnutls_hmac()` to feed bytes into the HMAC.
///    `gnutls_hmac()` reads a function pointer from a fixed offset in
///    the handle and calls it. That slot now holds arbitrary heap
///    bytes, the call jumps to an invalid address, and the kernel
///    raises `SIGSEGV`.
///
/// # Solution in short
///
/// Force `libgiognutls.so` to run its own initialization at startup.
/// That initialization calls `gnutls_global_init()` for us, before
/// `libnice` ever needs `libgnutls`.
///
/// # Solution in detail
///
/// `gio::TlsBackend::default()` calls GIO's `g_tls_backend_get_default()`.
/// On the first call, that function:
///
/// 1. Scans `gio/modules/` and loads every module it finds, including
///    `libgiognutls.so`.
/// 2. Looks up the `GTlsBackendGnutls` `GType` registered by that
///    module.
/// 3. Constructs an instance using `g_initable_new()`. That call
///    invokes the backend's `GInitable::init` virtual function, which
///    calls `gnutls_global_init()`. The call is wrapped in `g_once`,
///    so it runs exactly once for the lifetime of the process.
///
/// After this returns, `libgnutls`'s state is `LIB_STATE_INIT`,
/// `gnutls_hmac_init()` fills HMAC handles correctly, and `libnice`'s
/// STUN authentication works.
///
/// On systems where the original bug cannot occur, the call is
/// harmless. If `glib-networking` is not installed, GIO returns a
/// no-op fallback backend, and `libgnutls` (whose
/// `_gnutls_global_init_skip` override is missing) is auto-initialized
/// the normal way. If a non-GnuTLS backend is the default, GIO
/// initializes that one instead, and `libgnutls` is either not loaded
/// at all or, if `libnice` loads it, has no override stopping its own
/// auto-initialization.
#[instrument(level = "debug")]
pub fn ensure_tls_backend_initialized() {
    let backend = gio::TlsBackend::default();
    debug!(
        "GIO TLS backend constructed (supports_tls: {tls}, supports_dtls: {dtls})",
        tls = backend.supports_tls(),
        dtls = backend.supports_dtls(),
    );
}
