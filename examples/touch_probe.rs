//! Passive timing probe for HID contacts versus generated mouse movement.
//! Runs for 60 seconds; does not inject input or change the active controller.
#![allow(dead_code)]
#![windows_subsystem = "windows"]
#[path = "../src/touchpad.rs"]
mod touchpad;
use serde_json::json;
use std::{
    cell::RefCell,
    fs::File,
    io::{BufWriter, Write},
    mem::{size_of, zeroed},
    ptr::{null, null_mut},
    time::Instant,
};
use windows_sys::Win32::{
    Foundation::*,
    System::LibraryLoader::GetModuleHandleW,
    UI::{Input::*, WindowsAndMessaging::*},
};
struct Probe {
    pad: touchpad::Touchpad,
    file: BufWriter<File>,
    start: Instant,
    cursor: Option<(i32, i32, u32)>,
}
thread_local! {static PROBE:RefCell<Option<Probe>>=const{RefCell::new(None)};}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_INPUT => {
            PROBE.with(|p| { if let Some(p)=p.borrow_mut().as_mut() {
                let time=p.start.elapsed().as_micros();
                let mut header:RAWINPUTHEADER=zeroed();
                let mut size=size_of::<RAWINPUTHEADER>() as u32;
                if GetRawInputData(l as HRAWINPUT,RID_HEADER,&mut header as *mut _ as *mut _,&mut size,size_of::<RAWINPUTHEADER>() as u32)==u32::MAX{return;}
                let event=if header.dwType==RIM_TYPEHID {
                    json!({"us":time,"kind":"hid","frames":p.pad.input(l,false)})
                } else if header.dwType==RIM_TYPEMOUSE {
                    let mut raw:RAWINPUT=zeroed();let mut size=size_of::<RAWINPUT>() as u32;
                    if GetRawInputData(l as HRAWINPUT,RID_INPUT,&mut raw as *mut _ as *mut _,&mut size,size_of::<RAWINPUTHEADER>() as u32)==u32::MAX{return;}
                    let m=raw.data.mouse;
                    let slide=p.pad.motion(m.lLastX!=0 || m.lLastY!=0,m.Anonymous.Anonymous.usButtonFlags!=0);
                    json!({"us":time,"kind":"mouse","dx":m.lLastX,"dy":m.lLastY,"buttons":m.Anonymous.Anonymous.usButtonFlags,"slide":slide})
                } else {return;};
                let _=writeln!(p.file,"{event}");
            }});
            DefWindowProcW(hwnd, msg, w, l)
        }
        WM_TIMER => {
            let done = PROBE.with(|p| {
                let mut p = p.borrow_mut();
                let p = p.as_mut().unwrap();
                let mut cursor: CURSORINFO = zeroed();
                cursor.cbSize = size_of::<CURSORINFO>() as u32;
                if GetCursorInfo(&mut cursor) != 0 {
                    let next = (cursor.ptScreenPos.x, cursor.ptScreenPos.y, cursor.flags);
                    if p.cursor != Some(next) {
                        p.cursor = Some(next);
                        let event = json!({"us":p.start.elapsed().as_micros(),"kind":"cursor","xy":[next.0,next.1],"flags":next.2});
                        let _ = writeln!(p.file,"{event}");
                    }
                }
                let _ = p.file.flush();
                p.start.elapsed().as_secs() >= 60
            });
            if done {
                DestroyWindow(hwnd);
            }
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}
fn main() {
    unsafe {
        std::fs::create_dir_all("recordings").unwrap();
        let file = File::create(format!(
            "recordings/touch-probe-{}.jsonl",
            std::process::id()
        ))
        .unwrap();
        PROBE.with(|p| {
            *p.borrow_mut() = Some(Probe {
                pad: touchpad::Touchpad::default(),
                file: BufWriter::new(file),
                start: Instant::now(),
                cursor: None,
            })
        });
        let name = wide("TouchTimingProbe");
        let instance = GetModuleHandleW(null());
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            lpszClassName: name.as_ptr(),
            ..zeroed()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            0,
            name.as_ptr(),
            name.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            null_mut(),
            instance,
            null(),
        );
        assert!(!hwnd.is_null());
        assert!(touchpad::Touchpad::register(hwnd));
        let dev = RAWINPUTDEVICE {
            usUsagePage: 1,
            usUsage: 2,
            dwFlags: RIDEV_INPUTSINK,
            hwndTarget: hwnd,
        };
        assert_ne!(
            RegisterRawInputDevices(&dev, 1, size_of::<RAWINPUTDEVICE>() as u32),
            0
        );
        SetTimer(hwnd, 1, 16, None);
        let mut msg = zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        PROBE.with(|p| p.borrow_mut().take());
    }
}
