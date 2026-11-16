//! The channel between `iph` and its daemon: a Unix socket on macOS, a named
//! pipe on Windows. Either way one connection carries one request line and
//! one reply line.

use tokio::io::{AsyncRead, AsyncWrite};

pub trait Duplex: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Duplex for T {}

pub type Stream = Box<dyn Duplex>;

pub use imp::{Listener, cleanup, connect};

#[cfg(unix)]
mod imp {
    use std::os::unix::fs::PermissionsExt;

    use anyhow::{Context, Result};
    use tokio::net::{UnixListener, UnixStream};

    use super::Stream;
    use crate::paths;

    pub async fn connect(udid: &str) -> std::io::Result<Stream> {
        Ok(Box::new(UnixStream::connect(paths::socket(udid)).await?))
    }

    pub struct Listener(UnixListener);

    impl Listener {
        pub fn bind(udid: &str) -> Result<Self> {
            let sock = paths::socket(udid);
            let _ = std::fs::remove_file(&sock);
            let listener = UnixListener::bind(&sock).with_context(|| format!("bind {}", sock.display()))?;
            std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600))?;
            Ok(Self(listener))
        }

        pub async fn accept(&mut self) -> std::io::Result<Stream> {
            let (stream, _) = self.0.accept().await?;
            Ok(Box::new(stream))
        }
    }

    /// Remove what `bind` left behind.
    pub fn cleanup(udid: &str) {
        let _ = std::fs::remove_file(paths::socket(udid));
    }
}

#[cfg(windows)]
mod imp {
    use std::{ffi::c_void, time::Duration};

    use anyhow::{Context, Result};
    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions};
    use windows_sys::Win32::Security::{
        Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1},
        PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
    };

    use super::Stream;

    /// ERROR_PIPE_BUSY: every instance is taken; the server makes a new one
    /// as soon as it accepts.
    const PIPE_BUSY: i32 = 231;

    fn name(udid: &str) -> String {
        format!(r"\\.\pipe\iph-{udid}")
    }

    pub async fn connect(udid: &str) -> std::io::Result<Stream> {
        for _ in 0..100 {
            match ClientOptions::new().open(name(udid)) {
                Ok(client) => return Ok(Box::new(client)),
                Err(e) if e.raw_os_error() == Some(PIPE_BUSY) => tokio::time::sleep(Duration::from_millis(20)).await,
                Err(e) => return Err(e),
            }
        }
        Err(std::io::Error::other("the daemon's pipe stayed busy"))
    }

    /// Full access for the pipe's owner, SYSTEM and Administrators only; the
    /// default DACL would also let Everyone and Anonymous open it for reading.
    const PIPE_SDDL: &str = "D:P(A;;GA;;;OW)(A;;GA;;;SY)(A;;GA;;;BA)";

    /// A security descriptor built once in `bind` and never freed.
    struct Descriptor(PSECURITY_DESCRIPTOR);

    // SAFETY: the pointer refers to a self-relative descriptor that is
    // allocated once, intentionally leaked (never `LocalFree`d) and never
    // written after creation; Windows only reads it when a pipe instance is
    // created. Sharing it between threads is therefore sound.
    unsafe impl Send for Descriptor {}
    unsafe impl Sync for Descriptor {}

    fn owner_only_descriptor() -> Option<Descriptor> {
        let sddl: Vec<u16> = PIPE_SDDL.encode_utf16().chain(std::iter::once(0)).collect();
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `sddl` is NUL-terminated UTF-16 that outlives the call, `sd`
        // is a valid out pointer, and the size out pointer is optional.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 || sd.is_null() {
            tracing::warn!(
                "pipe security descriptor: {}; using the default",
                std::io::Error::last_os_error()
            );
            return None;
        }
        Some(Descriptor(sd))
    }

    /// Create one pipe instance, owner-only when the descriptor exists.
    fn create(options: &ServerOptions, name: &str, sd: Option<&Descriptor>) -> std::io::Result<NamedPipeServer> {
        let Some(sd) = sd else {
            return options.create(name);
        };
        let mut attrs = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0,
            bInheritHandle: 0,
        };
        // SAFETY: `attrs` is a valid SECURITY_ATTRIBUTES that lives for the
        // whole call, and its descriptor is valid and never freed.
        unsafe { options.create_with_security_attributes_raw(name, &mut attrs as *mut SECURITY_ATTRIBUTES as *mut c_void) }
    }

    pub struct Listener {
        name: String,
        next: NamedPipeServer,
        sd: Option<Descriptor>,
    }

    impl Listener {
        pub fn bind(udid: &str) -> Result<Self> {
            let name = name(udid);
            let sd = owner_only_descriptor();
            // First instance only: a second daemon for the same phone fails here.
            let mut options = ServerOptions::new();
            options.first_pipe_instance(true).reject_remote_clients(true);
            let next = create(&options, &name, sd.as_ref()).with_context(|| format!("create {name}"))?;
            Ok(Self { name, next, sd })
        }

        pub async fn accept(&mut self) -> std::io::Result<Stream> {
            self.next.connect().await?;
            let mut options = ServerOptions::new();
            options.reject_remote_clients(true);
            let fresh = create(&options, &self.name, self.sd.as_ref())?;
            Ok(Box::new(std::mem::replace(&mut self.next, fresh)))
        }
    }

    /// A pipe goes away with its process; nothing to remove.
    pub fn cleanup(_udid: &str) {}
}
