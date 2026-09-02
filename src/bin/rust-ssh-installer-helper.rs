#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod windows_impl {
    use std::ffi::OsString;
    use std::mem::{size_of, zeroed};
    use std::os::windows::ffi::OsStringExt;
    use std::ptr::null_mut;
    use std::thread::sleep;
    use std::time::{Duration, Instant};

    type Bool = i32;
    type Dword = u32;
    type Handle = *mut core::ffi::c_void;
    type Hwnd = *mut core::ffi::c_void;

    const INVALID_HANDLE_VALUE: Handle = -1isize as Handle;
    const TH32CS_SNAPPROCESS: Dword = 0x0000_0002;
    const PROCESS_TERMINATE: Dword = 0x0000_0001;
    const SYNCHRONIZE: Dword = 0x0010_0000;
    const MB_OKCANCEL: u32 = 0x0000_0001;
    const MB_ICONWARNING: u32 = 0x0000_0030;
    const MB_ICONERROR: u32 = 0x0000_0010;
    const IDOK: i32 = 1;

    #[repr(C)]
    struct ProcessEntry32W {
        dw_size: Dword,
        cnt_usage: Dword,
        th32_process_id: Dword,
        th32_default_heap_id: usize,
        th32_module_id: Dword,
        cnt_threads: Dword,
        th32_parent_process_id: Dword,
        pc_pri_class_base: i32,
        dw_flags: Dword,
        sz_exe_file: [u16; 260],
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CloseHandle(handle: Handle) -> Bool;
        fn CreateToolhelp32Snapshot(flags: Dword, process_id: Dword) -> Handle;
        fn OpenProcess(desired_access: Dword, inherit_handle: Bool, process_id: Dword) -> Handle;
        fn Process32FirstW(snapshot: Handle, entry: *mut ProcessEntry32W) -> Bool;
        fn Process32NextW(snapshot: Handle, entry: *mut ProcessEntry32W) -> Bool;
        fn TerminateProcess(process: Handle, exit_code: Dword) -> Bool;
        fn WaitForSingleObject(handle: Handle, milliseconds: Dword) -> Dword;
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBoxW(window: Hwnd, text: *const u16, caption: *const u16, kind: u32) -> i32;
    }

    enum CloseError {
        Cancelled,
        Failed(String),
    }

    pub fn run(target: &str, display_name: &str) -> i32 {
        match close_if_running(target, display_name) {
            Ok(()) => 0,
            Err(CloseError::Cancelled) => 1602,
            Err(CloseError::Failed(reason)) => {
                show_message(
                    &format!(
                        "无法关闭 {display_name}。\n\n安装已取消，未替换程序文件。\n请手动退出后重新运行 MSI。\n\n原因：{reason}"
                    ),
                    "Rust-SSH 安装程序",
                    MB_ICONERROR,
                );
                1603
            }
        }
    }

    fn close_if_running(target: &str, display_name: &str) -> Result<(), CloseError> {
        let pids = process_ids(target).map_err(CloseError::Failed)?;
        if pids.is_empty() {
            return Ok(());
        }

        let prompt = format!(
            "检测到 {display_name} 正在运行。\n\n点击“确定”将强制关闭它并继续安装。\n点击“取消”退出安装，不会关闭程序。"
        );
        if show_message(&prompt, "Rust-SSH 安装程序", MB_OKCANCEL | MB_ICONWARNING) != IDOK {
            return Err(CloseError::Cancelled);
        }

        for pid in pids {
            terminate_process(pid);
        }

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = process_ids(target).map_err(CloseError::Failed)?;
            if remaining.is_empty() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(CloseError::Failed(format!(
                    "仍有 {} 个进程未退出",
                    remaining.len()
                )));
            }
            sleep(Duration::from_millis(100));
        }
    }

    fn process_ids(target: &str) -> Result<Vec<Dword>, String> {
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot.is_null() || snapshot == INVALID_HANDLE_VALUE {
            return Err("无法枚举 Windows 进程".to_owned());
        }

        let mut entry: ProcessEntry32W = unsafe { zeroed() };
        entry.dw_size = size_of::<ProcessEntry32W>() as Dword;
        let mut ids = Vec::new();
        let mut has_entry = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
        while has_entry {
            let name = utf16_name(&entry.sz_exe_file);
            if name.eq_ignore_ascii_case(target) && !ids.contains(&entry.th32_process_id) {
                ids.push(entry.th32_process_id);
            }
            has_entry = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
        }
        unsafe {
            CloseHandle(snapshot);
        }
        Ok(ids)
    }

    fn terminate_process(pid: Dword) {
        let process = unsafe { OpenProcess(PROCESS_TERMINATE | SYNCHRONIZE, 0, pid) };
        if process.is_null() {
            return;
        }
        unsafe {
            TerminateProcess(process, 1);
            let _ = WaitForSingleObject(process, 5_000);
            CloseHandle(process);
        }
    }

    fn utf16_name(value: &[u16]) -> String {
        let length = value
            .iter()
            .position(|character| *character == 0)
            .unwrap_or(value.len());
        OsString::from_wide(&value[..length])
            .to_string_lossy()
            .into_owned()
    }

    fn show_message(text: &str, caption: &str, kind: u32) -> i32 {
        let text = wide(text);
        let caption = wide(caption);
        unsafe { MessageBoxW(null_mut(), text.as_ptr(), caption.as_ptr(), kind) }
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }
}

#[cfg(windows)]
fn main() {
    let target = std::env::args().nth(1).unwrap_or_default();
    let (target, display_name) = match target.as_str() {
        "client" => ("rust-ssh-client.exe", "Rust-SSH-Client"),
        "connect" => ("rust-ssh-connect.exe", "Rust-SSH-Connect"),
        _ => {
            std::process::exit(1603);
        }
    };
    std::process::exit(windows_impl::run(target, display_name));
}

#[cfg(not(windows))]
fn main() {}
