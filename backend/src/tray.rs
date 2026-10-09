use tokio_util::sync::CancellationToken;

use crate::player;

#[cfg(target_os = "windows")]
pub fn run(port: u16, player: player::Shared, shutdown: CancellationToken) {
    use std::time::{Duration, Instant};

    use tray_icon::menu::{Menu, MenuEvent, MenuItem};
    use tray_icon::{Icon, TrayIconBuilder};
    use windows::core::{w, HSTRING, PCWSTR};
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE, SW_SHOW,
    };

    fn open_in_browser(url: &str) {
        unsafe {
            let _ = ShellExecuteW(
                None,
                w!("open"),
                &HSTRING::from(url),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOW,
            );
        }
    }

    fn make_icon() -> Icon {
        const SIZE: usize = 32;
        let mut rgba = vec![0u8; SIZE * SIZE * 4];
        let center = SIZE as f32 / 2.0;
        let radius = 13.0f32;
        for y in 0..SIZE {
            for x in 0..SIZE {
                let dx = x as f32 + 0.5 - center;
                let dy = y as f32 + 0.5 - center;
                if dx * dx + dy * dy <= radius * radius {
                    let i = (y * SIZE + x) * 4;
                    rgba[i] = 56;
                    rgba[i + 1] = 189;
                    rgba[i + 2] = 248;
                    rgba[i + 3] = 255;
                }
            }
        }
        Icon::from_rgba(rgba, SIZE as u32, SIZE as u32).expect("failed to build tray icon image")
    }

    std::thread::spawn(move || {
        let menu = Menu::new();
        let open_item = MenuItem::new("Open settings", true, None);
        let quit_item = MenuItem::new("Quit", true, None);
        let open_id = open_item.id().clone();
        let quit_id = quit_item.id().clone();
        let _ = menu.append(&open_item);
        let _ = menu.append(&quit_item);

        let tray = match TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("music-player")
            .with_icon(make_icon())
            .build()
        {
            Ok(t) => t,
            Err(e) => {
                eprintln!("failed to create tray icon: {e}");
                return;
            }
        };

        let shutdown_handler = shutdown.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if event.id == open_id {
                open_in_browser(&format!("http://127.0.0.1:{port}"));
            } else if event.id == quit_id {
                println!("shutdown requested via tray menu");
                shutdown_handler.cancel();
            }
        }));

        // win32 message loop with periodic tasks; tray-icon requires the
        // loop on the same thread that created the icon
        let mut last_tooltip = Instant::now() - Duration::from_secs(10);
        unsafe {
            loop {
                if shutdown.is_cancelled() {
                    break;
                }
                let mut msg = MSG::default();
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                if last_tooltip.elapsed() >= Duration::from_secs(3) {
                    last_tooltip = Instant::now();
                    let text = {
                        let s = player.read().expect("player state poisoned");
                        match &s.current_track {
                            Some(t) => format!("music-player: {t}"),
                            None => "music-player".to_string(),
                        }
                    };
                    let _ = tray.set_tooltip(Some(&text));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
        drop(tray);
        println!("tray icon removed");
    });
}

#[cfg(not(target_os = "windows"))]
pub fn run(_port: u16, _player: player::Shared, _shutdown: CancellationToken) {
    eprintln!("tray icon is only supported on Windows in this build");
}
