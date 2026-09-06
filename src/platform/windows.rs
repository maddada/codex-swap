use anyhow::{Context, Result, bail};
use std::{ffi::OsStr, fs::File, os::windows::ffi::OsStrExt, path::Path, process::Command, ptr};
use windows_sys::Win32::{
    Foundation::{CloseHandle, LocalFree},
    Security::{
        Authorization::{
            EXPLICIT_ACCESS_W, GetNamedSecurityInfoW, SE_FILE_OBJECT, SET_ACCESS, SetEntriesInAclW,
            SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
        },
        DACL_SECURITY_INFORMATION, EqualSid, GetTokenInformation, NO_INHERITANCE,
        OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
        SUB_CONTAINERS_AND_OBJECTS_INHERIT, TOKEN_INFORMATION_CLASS, TOKEN_OWNER, TOKEN_QUERY,
        TOKEN_USER, TokenOwner, TokenUser,
    },
    Storage::FileSystem::FILE_ALL_ACCESS,
    System::{
        Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler},
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        },
        Threading::{GetCurrentProcess, OpenProcessToken},
    },
};

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}

fn token_information(kind: TOKEN_INFORMATION_CLASS) -> Result<Vec<usize>> {
    unsafe {
        let mut token = ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(std::io::Error::last_os_error()).context("read Windows account identity");
        }
        let mut size = 0;
        GetTokenInformation(token, kind, ptr::null_mut(), 0, &mut size);
        let mut data = vec![0usize; (size as usize).div_ceil(size_of::<usize>())];
        let result = GetTokenInformation(token, kind, data.as_mut_ptr().cast(), size, &mut size);
        let error = std::io::Error::last_os_error();
        CloseHandle(token);
        if result == 0 {
            return Err(error).context("read Windows account SID");
        }
        Ok(data)
    }
}

/// Set the owner on a newly created object before storing data there.
/// Elevated Windows tokens can otherwise assign the Administrators group as owner.
pub fn own_new(path: &Path) -> Result<()> {
    let current = token_information(TokenUser)?;
    unsafe {
        let sid = (*(current.as_ptr().cast::<TOKEN_USER>())).User.Sid;
        let error = SetNamedSecurityInfoW(
            wide(path.as_os_str()).as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            sid,
            ptr::null_mut(),
            ptr::null(),
            ptr::null(),
        );
        if error != 0 {
            return Err(std::io::Error::from_raw_os_error(error as i32))
                .with_context(|| format!("set owner of new {}", path.display()));
        }
    }
    Ok(())
}

pub fn owned(path: &Path) -> Result<()> {
    let current = token_information(TokenUser)?;
    let default_owner = token_information(TokenOwner)?;
    unsafe {
        let sid = (*(current.as_ptr().cast::<TOKEN_USER>())).User.Sid;
        let mut owner = ptr::null_mut();
        let mut descriptor = ptr::null_mut();
        let error = GetNamedSecurityInfoW(
            wide(path.as_os_str()).as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut descriptor,
        );
        if error != 0 {
            return Err(std::io::Error::from_raw_os_error(error as i32))
                .with_context(|| format!("read owner of {}", path.display()));
        }
        // An elevated Codex process can assign auth.json to its token's default owner.
        // Accept that exact owner SID, rather than broadening access to arbitrary token groups.
        let default_sid = (*(default_owner.as_ptr().cast::<TOKEN_OWNER>())).Owner;
        let matches =
            !owner.is_null() && (EqualSid(owner, sid) != 0 || EqualSid(owner, default_sid) != 0);
        LocalFree(descriptor);
        if !matches {
            bail!("{} must be owned by your Windows account", path.display());
        }
    }
    Ok(())
}

/// CDXC:AgentProviders 2026-09-06 WHY:
/// Windows ignores Unix mode bits, so credential storage uses a protected owner-only DACL before any secret is written.
pub fn private_permissions(path: &Path, directory: bool) -> Result<()> {
    owned(path)?;
    let current = token_information(TokenUser)?;
    unsafe {
        let sid = (*(current.as_ptr().cast::<TOKEN_USER>())).User.Sid;
        let entry = EXPLICIT_ACCESS_W {
            grfAccessPermissions: FILE_ALL_ACCESS,
            grfAccessMode: SET_ACCESS,
            grfInheritance: if directory {
                SUB_CONTAINERS_AND_OBJECTS_INHERIT
            } else {
                NO_INHERITANCE
            },
            Trustee: TRUSTEE_W {
                pMultipleTrustee: ptr::null_mut(),
                MultipleTrusteeOperation: 0,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_USER,
                ptstrName: sid.cast(),
            },
        };
        let mut acl = ptr::null_mut();
        let error = SetEntriesInAclW(1, &entry, ptr::null(), &mut acl);
        if error != 0 {
            return Err(std::io::Error::from_raw_os_error(error as i32))
                .context("build private Windows ACL");
        }
        let error = SetNamedSecurityInfoW(
            wide(path.as_os_str()).as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            acl,
            ptr::null(),
        );
        LocalFree(acl.cast());
        if error != 0 {
            return Err(std::io::Error::from_raw_os_error(error as i32))
                .with_context(|| format!("protect {} with a private Windows ACL", path.display()));
        }
    }
    Ok(())
}

pub fn keep_lease_across_exec(_file: &File) -> Result<()> {
    // Windows keeps this process alive, and its lease stays held until the child exits.
    Ok(())
}

pub fn codex_command(binary: &OsStr) -> Result<Command> {
    let path = Path::new(binary);
    let directories: Vec<_> = if path.components().count() > 1 || path.is_absolute() {
        vec![std::path::PathBuf::new()]
    } else {
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect()
    };
    for directory in directories {
        let candidate = directory.join(path);
        let candidates = if path.extension().is_some() {
            vec![candidate]
        } else {
            // npm installs a codex.cmd shim alongside an extensionless Unix script.
            vec![
                candidate.with_extension("exe"),
                candidate.with_extension("cmd"),
                candidate.with_extension("bat"),
            ]
        };
        for candidate in candidates {
            if candidate.is_file() {
                return Ok(Command::new(candidate));
            }
        }
    }
    bail!(
        "could not find Codex on PATH; install the official Codex CLI or set XSWAP_CODEX_BIN to codex.exe or codex.cmd"
    )
}

/// CDXC:AgentProviders 2026-09-06 WHY:
/// Windows cannot exec Codex, so parent termination must also end Codex before its account lease becomes available.
/// Assign xswap to a kill-on-close job before spawning, making children inherit membership without a startup race.
/// The noninheritable handle stays open for the process lifetime; normal exit also ends any remaining spawned descendants.
fn account_process_job() -> Result<()> {
    static JOB: std::sync::OnceLock<std::result::Result<usize, std::io::Error>> =
        std::sync::OnceLock::new();
    let result = JOB.get_or_init(|| unsafe {
        let job = CreateJobObjectW(ptr::null(), ptr::null());
        if job.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) == 0
        {
            let error = std::io::Error::last_os_error();
            CloseHandle(job);
            return Err(error);
        }
        if AssignProcessToJobObject(job, GetCurrentProcess()) == 0 {
            let error = std::io::Error::last_os_error();
            CloseHandle(job);
            return Err(error);
        }
        // This handle is deliberately never closed by Rust: closing it kills xswap too.
        // Windows closes it during process teardown, including forced termination.
        Ok(job as usize)
    });
    match result {
        Ok(_) => Ok(()),
        Err(error) => {
            bail!("cannot establish Windows account process lifetime protection: {error}")
        }
    }
}

unsafe extern "system" fn child_console_event(event: u32) -> i32 {
    // The child receives the same console event. Keep the parent and account lease alive.
    i32::from(event == CTRL_C_EVENT || event == CTRL_BREAK_EVENT)
}

pub fn execute(mut cmd: Command, _lease: File) -> Result<()> {
    let status = status(&mut cmd)?;
    std::process::exit(status.code().unwrap_or(1));
}

pub fn status(cmd: &mut Command) -> Result<std::process::ExitStatus> {
    account_process_job()?;
    unsafe {
        if SetConsoleCtrlHandler(Some(child_console_event), 1) == 0 {
            return Err(std::io::Error::last_os_error())
                .context("retain account lease during console events");
        }
    }
    let status = cmd
        .status()
        .context("could not start Codex; install it or set XSWAP_CODEX_BIN");
    unsafe {
        SetConsoleCtrlHandler(Some(child_console_event), 0);
    }
    status
}
