//! Locks a running program to what its capabilities need, on Linux: the operating system then
//! refuses the rest, even to code that slipped past the build checks.
//!
//! The wiring builds the capabilities, asks each what it reaches ([`Grants`]), and locks before
//! the app runs:
//!
//! ```ignore
//! Lockdown::new().allow(&folder).allow(&http).apply()?;
//! ```
//!
//! - **Files** (Landlock): only the files and folders granted, read or read-write. Everything
//!   else on the disk is refused, reading included.
//! - **TCP** (Landlock): connecting and listening only on the ports granted.
//! - **Signals and abstract sockets** (Landlock scopes): only within the program.
//! - **System calls** (seccomp): never starting programs, debugging other
//!   processes, loading kernel code or mounting; with no network grant, no internet sockets at
//!   all, UDP included.
//!
//! Lock before starting threads: a lock covers the thread that applies it and what it starts
//! afterwards. Once applied it can't be lifted or widened. On other systems [`Lockdown::apply`]
//! locks nothing and says so; on an older Linux it locks what that kernel supports.
#![forbid(unsafe_code)]

use std::fmt;
use std::path::{Path, PathBuf};

/// One thing a capability needs from the operating system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Grant {
    /// Read this file, or everything beneath this folder.
    Read(PathBuf),
    /// Read, create, change and delete beneath this folder (or change this file).
    ReadWrite(PathBuf),
    /// Open TCP connections to this port, on any host. Which hosts is the capability's scope.
    Connect(u16),
    /// Listen for TCP connections on this port.
    Listen(u16),
    /// Look up names: the system resolver's files, libraries and socket, and UDP.
    Resolve,
    /// Internet sockets for UDP (any port: Landlock doesn't cover UDP), for discovery on the
    /// local network.
    Udp,
    /// Use a terminal device opened by name (`/dev/tty`).
    Terminal,
}

/// What a capability reaches. Implemented by the native capabilities; the wiring passes them to
/// [`Lockdown::allow`].
pub trait Grants {
    fn grants(&self) -> Vec<Grant>;
}

impl<T: Grants> Grants for [T] {
    fn grants(&self) -> Vec<Grant> {
        self.iter().flat_map(T::grants).collect()
    }
}

impl<T: Grants> Grants for Vec<T> {
    fn grants(&self) -> Vec<Grant> {
        self.as_slice().grants()
    }
}

impl<T: Grants> Grants for Option<T> {
    fn grants(&self) -> Vec<Grant> {
        self.iter().flat_map(T::grants).collect()
    }
}

impl Grants for Grant {
    fn grants(&self) -> Vec<Grant> {
        vec![self.clone()]
    }
}

/// One file opened by the wiring for the app: it may be read, nothing else in its folder.
impl Grants for ambit_caps_dir::OneFile {
    fn grants(&self) -> Vec<Grant> {
        self.origin()
            .map(|path| Grant::Read(path.to_path_buf()))
            .into_iter()
            .collect()
    }
}

/// What the program will be locked to.
#[derive(Debug, Default, Clone)]
pub struct Lockdown {
    grants: Vec<Grant>,
}

impl Lockdown {
    /// Nothing granted: no files, no network, no programs.
    pub fn new() -> Self {
        Self::default()
    }

    pub fn allow(mut self, caps: &(impl Grants + ?Sized)) -> Self {
        self.grants.extend(caps.grants());
        self
    }

    /// Creates the folder `path` (and its parents) now, and grants it read-write. For a folder
    /// the app will write into but that may not exist yet: a lock covers only what is there when
    /// it's applied.
    pub fn make(mut self, path: impl AsRef<Path>) -> Result<Self, LockError> {
        let path = path.as_ref();
        std::fs::create_dir_all(path).map_err(|e| LockError(format!("{}: {e}", path.display())))?;
        self.grants.push(Grant::ReadWrite(path.to_path_buf()));
        Ok(self)
    }

    pub fn grants(&self) -> &[Grant] {
        &self.grants
    }

    /// [`Lockdown::apply`] for `main`: says on stderr, as `program`, when the lock isn't
    /// complete on Linux. The error is for `main` to stop on.
    pub fn apply_or_warn(self, program: &str) -> Result<(), LockError> {
        if let Some(warning) = self.apply()?.warning() {
            eprintln!("{program}: {warning}");
        }
        Ok(())
    }

    /// Locks this process. Fails, and locks nothing, if a grant can't be applied (a path that
    /// isn't there is skipped: there's nothing to reach).
    pub fn apply(self) -> Result<Locked, LockError> {
        imp::apply(&self.grants)
    }
}

/// How far the lock went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Locked {
    /// Everything above is enforced.
    Fully,
    /// This kernel enforces part of it: what's missing is in the message.
    Partly(String),
    /// Nothing is locked on this system.
    Not(String),
}

impl Locked {
    /// A line for the user when the lock isn't complete on Linux; `None` otherwise. Other systems
    /// stay quiet: there it's expected.
    pub fn warning(&self) -> Option<String> {
        match self {
            Locked::Fully => None,
            Locked::Partly(why) => Some(format!("lockdown: only partly enforced: {why}")),
            Locked::Not(why) if cfg!(target_os = "linux") => {
                Some(format!("lockdown: not enforced: {why}"))
            }
            Locked::Not(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockError(pub String);

impl fmt::Display for LockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "lockdown: {}", self.0)
    }
}

impl std::error::Error for LockError {}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::*;

    pub fn apply(_: &[Grant]) -> Result<Locked, LockError> {
        Ok(Locked::Not(
            "only Linux locks programs (Landlock, seccomp)".into(),
        ))
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::collections::BTreeMap;

    use landlock::{
        ABI, Access, AccessFs, AccessNet, BitFlags, NetPort, PathBeneath, PathFd, Ruleset,
        RulesetAttr, RulesetCreatedAttr, RulesetStatus, Scope, make_bitflags,
    };
    use seccompiler::{
        SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
        SeccompRule, TargetArch,
    };

    use super::*;

    /// The newest Landlock this code knows. Older kernels enforce what they have.
    const ABI_KNOWN: ABI = ABI::V9;

    /// What the system resolver reads: its configuration, the libraries it loads for a lookup,
    /// and where systemd-resolved and nscd answer.
    const RESOLVER_FILES: &[&str] = &[
        "/etc/resolv.conf",
        "/etc/hosts",
        "/etc/nsswitch.conf",
        "/etc/gai.conf",
        "/etc/host.conf",
        "/etc/services",
        "/etc/ld.so.cache",
    ];
    const RESOLVER_LIBS: &[&str] = &["/usr/lib64", "/usr/lib", "/lib64", "/lib"];
    // avahi: `.local` names (nss-mdns, as Fedora and Debian set it up).
    const RESOLVER_SOCKETS: &[&str] = &["/run/systemd/resolve", "/run/nscd", "/run/avahi-daemon"];

    fn read() -> BitFlags<AccessFs> {
        make_bitflags!(AccessFs::{ReadFile | ReadDir})
    }

    fn read_write() -> BitFlags<AccessFs> {
        read()
            | make_bitflags!(AccessFs::{
                WriteFile | RemoveDir | RemoveFile | MakeDir | MakeReg | Refer | Truncate
            })
    }

    pub fn apply(grants: &[Grant]) -> Result<Locked, LockError> {
        let err = |what: &str, e: &dyn fmt::Display| LockError(format!("{what}: {e}"));
        let mut fs: Vec<(PathBuf, BitFlags<AccessFs>)> = Vec::new();
        let mut net: Vec<(u16, AccessNet)> = Vec::new();
        let mut inet = false;
        for grant in grants {
            match grant {
                Grant::Read(path) => fs.push((path.clone(), read())),
                Grant::ReadWrite(path) => fs.push((path.clone(), read_write())),
                Grant::Connect(port) => {
                    net.push((*port, AccessNet::ConnectTcp));
                    inet = true;
                }
                Grant::Listen(port) => {
                    net.push((*port, AccessNet::BindTcp));
                    inet = true;
                }
                Grant::Resolve => {
                    inet = true;
                    for path in RESOLVER_FILES {
                        fs.push((path.into(), AccessFs::ReadFile.into()));
                    }
                    for path in RESOLVER_LIBS {
                        fs.push((path.into(), read() | AccessFs::Execute));
                    }
                    for path in RESOLVER_SOCKETS {
                        fs.push((path.into(), read() | AccessFs::ResolveUnix));
                    }
                }
                Grant::Udp => inet = true,
                Grant::Terminal => fs.push((
                    "/dev/tty".into(),
                    make_bitflags!(AccessFs::{ReadFile | WriteFile | IoctlDev}),
                )),
            }
        }

        let mut ruleset = Ruleset::default()
            .handle_access(AccessFs::from_all(ABI_KNOWN))
            .and_then(|r| r.handle_access(AccessNet::from_all(ABI_KNOWN)))
            .and_then(|r| r.scope(Scope::from_all(ABI_KNOWN)))
            .and_then(|r| r.create())
            .map_err(|e| err("Landlock", &e))?;
        for (path, access) in &fs {
            // Nothing there, nothing to reach. A grant for a folder made later is `make`'s job.
            let Ok(meta) = std::fs::metadata(path) else {
                continue;
            };
            let access = if meta.is_dir() {
                *access
            } else {
                *access & AccessFs::from_file(ABI_KNOWN)
            };
            let fd = PathFd::new(path).map_err(|e| err(&path.display().to_string(), &e))?;
            ruleset = ruleset
                .add_rule(PathBeneath::new(fd, access))
                .map_err(|e| err(&path.display().to_string(), &e))?;
        }
        for (port, access) in &net {
            ruleset = ruleset
                .add_rule(NetPort::new(*port, *access))
                .map_err(|e| err(&format!("port {port}"), &e))?;
        }
        let status = ruleset.restrict_self().map_err(|e| err("Landlock", &e))?;

        syscalls(inet)?;

        Ok(match status.ruleset {
            RulesetStatus::FullyEnforced => Locked::Fully,
            RulesetStatus::PartiallyEnforced => Locked::Partly(format!(
                "this kernel's Landlock ({:?}) is older than {ABI_KNOWN:?}; system calls are \
                 locked",
                status.landlock
            )),
            RulesetStatus::NotEnforced => Locked::Partly(
                "this kernel has no Landlock, so files and TCP aren't locked; system calls are"
                    .into(),
            ),
        })
    }

    /// The system calls no program here needs, refused with "operation not permitted".
    const NEVER: &[i64] = &[
        libc::SYS_execve,
        libc::SYS_execveat,
        libc::SYS_ptrace,
        libc::SYS_process_vm_readv,
        libc::SYS_process_vm_writev,
        libc::SYS_mount,
        libc::SYS_umount2,
        libc::SYS_pivot_root,
        libc::SYS_chroot,
        libc::SYS_unshare,
        libc::SYS_setns,
        libc::SYS_bpf,
        libc::SYS_perf_event_open,
        libc::SYS_kexec_load,
        libc::SYS_kexec_file_load,
        libc::SYS_init_module,
        libc::SYS_finit_module,
        libc::SYS_delete_module,
        libc::SYS_reboot,
        libc::SYS_swapon,
        libc::SYS_swapoff,
        libc::SYS_add_key,
        libc::SYS_request_key,
        libc::SYS_keyctl,
        libc::SYS_userfaultfd,
        libc::SYS_io_uring_setup,
        libc::SYS_io_uring_enter,
        libc::SYS_io_uring_register,
        libc::SYS_open_by_handle_at,
        libc::SYS_name_to_handle_at,
        libc::SYS_fanotify_init,
    ];

    fn syscalls(inet: bool) -> Result<(), LockError> {
        let err = |e: &dyn fmt::Display| LockError(format!("seccomp: {e}"));
        let mut rules: BTreeMap<i64, Vec<SeccompRule>> =
            NEVER.iter().map(|call| (*call, Vec::new())).collect();
        if !inet {
            let family = |family: i32| {
                SeccompCondition::new(0, SeccompCmpArgLen::Dword, SeccompCmpOp::Eq, family as u64)
                    .and_then(|c| SeccompRule::new(vec![c]))
            };
            let socket = [libc::AF_INET, libc::AF_INET6, libc::AF_PACKET]
                .into_iter()
                .map(family)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| err(&e))?;
            rules.insert(libc::SYS_socket, socket);
        }
        let arch = TargetArch::try_from(std::env::consts::ARCH).map_err(|e| err(&e))?;
        let filter = SeccompFilter::new(
            rules,
            SeccompAction::Allow,
            SeccompAction::Errno(libc::EPERM as u32),
            arch,
        )
        .map_err(|e| err(&e))?;
        let program: seccompiler::BpfProgram = filter.try_into().map_err(|e| err(&e))?;
        seccompiler::apply_filter(&program).map_err(|e| err(&e))
    }
}
