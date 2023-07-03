mod byte_count;
mod mpris;

use std::{cmp::min, sync::Arc};

use chrono::Timelike;
use dbus_tokio::connection;
use serde::Deserialize;
use serde_json::json;
use tokio::{time::{Instant, Duration, timeout_at}, sync::Notify, io::{BufReader, stdin, AsyncBufReadExt}};
use sysinfo::{System, SystemExt, DiskExt};

use crate::{byte_count::ByteCount, mpris::Mpris};

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
struct ClickEvent {
    name: Option<String>,
    instance: Option<String>,
    x: i32,
    y: i32,
    button: i32,
    relative_x: i32,
    relative_y: i32,
    width: i32,
    height: i32,
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    println!("{}\n[", json!({
        "version": 1,
        "stop_signal": 0,
        "click_events": true,
    }));

    // NOTE: I'd rather use `new_session_local` but it leads to errors
    let (resource, conn) = connection::new_session_sync().unwrap();

    tokio::spawn(async {
        let err = resource.await;
        panic!("Lost connection to DBus: {}", err);
    });

    let invalidate = Arc::new(Notify::new());

    let mpris = Arc::new(Mpris::new(conn.clone(), "spotify", invalidate.clone()));

    let mpris2 = mpris.clone();
    tokio::spawn(async {
        let mpris = mpris2;
        let reader = BufReader::new(stdin());
        let mut lines = reader.lines();
        // ignore first line
        lines.next_line().await.unwrap();
        while let Some(line) = lines.next_line().await.unwrap() {
            let line = line.strip_prefix(',').unwrap_or(line.as_str());
            let event: ClickEvent = serde_json::from_str(line).unwrap();
            eprintln!("{:?}", event);
            if !event.name.is_some() {
                continue;
            }
            match event.name.unwrap().as_str() {
                "mpris-play" => if event.button == 1 { mpris.play() },
                "mpris-pause" => if event.button == 1 { mpris.pause() },
                "mpris-previous" => if event.button == 1 { mpris.previous() },
                "mpris-next" => if event.button == 1 { mpris.next() },
                _ => {}
            }
        }
    });

    let mut sys = System::new();

    loop {
        let current_track;
        let playing;
        {
            let state = mpris.state();
            if state.title.is_empty() {
                current_track = "".to_string();
            } else {
                current_track = format!("{} - {}", state.artists.join(" - "), state.title.clone());
            }
            playing = state.playing;
        }

        sys.refresh_disks_list();
        sys.refresh_disks();

        let disk_root = sys.disks().iter()
            .find(|&val| val.mount_point().as_os_str() == "/")
            .and_then(|disk| Some(format!("{:.2}", ByteCount::from(disk.available_space()))));

        let disk_home = sys.disks().iter()
            .find(|&val| val.mount_point().as_os_str() == "/home")
            .and_then(|disk| Some(format!("{:.2}", ByteCount::from(disk.available_space()))));

        let disk_hdd = sys.disks().iter()
            .find(|&val| val.mount_point().as_os_str() == "/srv")
            .and_then(|disk| Some(format!("{:.2}", ByteCount::from(disk.available_space()))));

        sys.refresh_memory();
        let memory = format!("M {:.2} S {:.2}",
            ByteCount::from(sys.available_memory()),
            ByteCount::from(sys.free_swap()));

        let now_monotonic = Instant::now();
        let now_wall = chrono::Local::now();

        println!("{},", json!([
            {
                "full_text": current_track,
                "separator": false,
            },
            {
                "full_text": if current_track.is_empty() { "" } else { "\u{f049}" },
                "name": "mpris-previous",
                "separator": false,
            },
            {
                "full_text": if current_track.is_empty() { "" } else
                    if playing { "\u{f04c}" } else { "\u{f04b}" },
                "name": if playing { "mpris-pause" } else { "mpris-play" },
                "separator": false,
            },
            {
                "full_text": if current_track.is_empty() { "" } else { "\u{f050}" },
                "name": "mpris-next",
            },
            {
                "full_text": format!("/ {}", disk_root.unwrap_or("ERROR".to_string())),
            },
            {
                "full_text": format!("/home {}", disk_home.unwrap_or("ERROR".to_string())),
            },
            {
                "full_text": format!("HDD {}", disk_hdd.unwrap_or("ERROR".to_string())),
            },
            {
                "full_text": memory,
            },
            {
                "full_text": now_wall.format("%a %d.%m.%Y %H:%M").to_string(),
            }
        ]));

        let next_minute_ms = 60000 - 1000 * now_wall.second() - now_wall.timestamp_subsec_millis();
        let wait_ms = min(2000, next_minute_ms);
        let wake_at = now_monotonic + Duration::from_millis(wait_ms.into());
        let _ = timeout_at(wake_at, invalidate.notified()).await;
    }
}
