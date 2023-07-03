use std::{sync::{Arc, Mutex, MutexGuard}, time::Duration};

use dbus::{nonblock::{Proxy, SyncConnection, MethodReply}, arg::{ReadAll, AppendAll, self, RefArg}, message::MatchRule};
use tokio::sync::Notify;

pub struct PlayerState {
    invalidate: Arc<Notify>,
    pub playing: bool,
    pub title: String,
    pub artists: Vec<String>,
}

impl PlayerState {
    fn new(invalidate: Arc<Notify>) -> Self {
        return Self {
            invalidate,
            playing: false,
            title: "".to_string(),
            artists: Vec::new(),
        };
    }

    fn name_lost(&mut self) {
        self.playing = false;
        self.title = "".to_string();
        self.artists.clear();
        self.invalidate.notify_one();
    }

    fn update_metadata(&mut self, metadata: Box<dyn RefArg>) {
        let mut iter = metadata.as_iter().unwrap();
        loop {
            let key = match iter.next() {
                Some(value) => value.as_str().unwrap(),
                None => break,
            };
            let value = iter.next().unwrap() // variant
                .as_iter().unwrap().next().unwrap(); // inner value of variant

            match key {
                "xesam:title" => {
                    self.title = value.as_str().unwrap().to_string();
                },
                "xesam:artist" => {
                    self.artists.clear();
                    for artist in value.as_iter().unwrap() {
                        self.artists.push(artist.as_str().unwrap().to_string());
                    }
                },
                _ => {
                    //eprintln!("{:?} -> {:?}", key, value);
                }
            }
        }
        self.invalidate.notify_one();
    }

    fn update(&mut self, props: arg::PropMap) {
        for (field, value) in props {
            match field.as_str() {
                "Metadata" => {
                    self.update_metadata(value.0);
                },
                "PlaybackStatus" => {
                    // Playing, Paused, Stopped
                    self.playing = value.as_str().unwrap() == "Playing";
                },
                _ => {
                    //eprintln!("{} -> {:?}", field, value);
                }
            }
        }
        self.invalidate.notify_one();
    }
}

pub struct Mpris<'a> {
    proxy: Proxy<'a, Arc<SyncConnection>>,
    destruct: Arc<Notify>,
    state: Arc<Mutex<PlayerState>>,
}

const INTERFACE: &'static str = "org.mpris.MediaPlayer2.Player";

impl<'a> Drop for Mpris<'a> {
    fn drop(&mut self) {
        self.destruct.notify_one();
    }
}

impl<'a> Mpris<'a> {
    pub fn new(conn: Arc<SyncConnection>, instance: &str, invalidate: Arc<Notify>) -> Self {
        let bus_name = format!("org.mpris.MediaPlayer2.{}", instance);
        let proxy = Proxy::new(
            bus_name.clone(),
            "/org/mpris/MediaPlayer2",
            Duration::from_secs(5),
            conn.clone());

        let destruct = Arc::new(Notify::new());
        let destruct2 = destruct.clone();

        let state = Arc::new(Mutex::new(PlayerState::new(invalidate)));
        let state2 = state.clone();

        let proxy2 = proxy.clone();

        tokio::spawn(async move {
            let state3 = state2.clone();
            let rule: MatchRule<'_> = MatchRule::new_signal("org.freedesktop.DBus.Properties", "PropertiesChanged")
                .with_sender(bus_name.clone());
            let r#match = conn.add_match(rule);
            let signal_property_changed = r#match.await.unwrap()
                .cb(move |_, (interface_name, changed_properties, invalidated_properties,): (String, arg::PropMap, Vec<String>)| {
                    if interface_name != INTERFACE {
                        return true;
                    }

                    if invalidated_properties.len() > 0 {
                        eprintln!("Unhandled PropertyChanged invalidated_properties.len() > 0");
                    }

                    state3.lock().unwrap().update(changed_properties);
                    true
                });

            let proxy3 = proxy2.clone();
            let state3 = state2.clone();
            let rule = MatchRule::new_signal("org.freedesktop.DBus", "NameOwnerChanged")
                .with_sender("org.freedesktop.DBus");
            let r#match = conn.add_match(rule);
            let signal_name_owner_changed = r#match.await.unwrap()
                .cb(move |_, (name, _old_owner, new_owner): (String, String, String)| {
                    if name != bus_name {
                        return true;
                    }

                    if new_owner.is_empty() {
                        state3.lock().unwrap().name_lost();
                    } else {
                        let state4 = state3.clone();
                        let proxy4 = proxy3.clone();
                        tokio::spawn(async move {
                            // Spotify on startup may take some time to get the song information and
                            // won't signal when it has them. So we wait a bit and ask for them manually.
                            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                            let (metadata,): (arg::Variant<Box<dyn RefArg + 'static>>,) = proxy4.method_call("org.freedesktop.DBus.Properties", "Get", (INTERFACE, "Metadata")).await.unwrap();
                            state4.lock().unwrap().update_metadata(metadata.0);
                        });
                    }
                    true
                });

            let (props,): (arg::PropMap,) = proxy2.method_call("org.freedesktop.DBus.Properties", "GetAll", (INTERFACE,)).await.unwrap();
            state2.lock().unwrap().update(props);

            destruct2.notified().await;

            conn.remove_match(signal_name_owner_changed.token()).await.unwrap();
            conn.remove_match(signal_property_changed.token()).await.unwrap();
        });

        return Self {
            proxy,
            destruct,
            state,
        };
    }

    fn send_call_simple<R, A>(&self, method: &'static str, args: A)
        where
            R: ReadAll + 'static,
            A: AppendAll,
    {
        let reply: MethodReply<R> = self.proxy.method_call(INTERFACE, method, args);
        tokio::spawn(async { let _ = reply.await; });
    }

    #[allow(dead_code)]
    pub fn play_pause(&self) { self.send_call_simple::<(), _>("PlayPause", ()); }

    #[allow(dead_code)]
    pub fn play(&self) { self.send_call_simple::<(), _>("Play", ()); }

    #[allow(dead_code)]
    pub fn pause(&self) { self.send_call_simple::<(), _>("Pause", ()); }

    #[allow(dead_code)]
    pub fn stop(&self) { self.send_call_simple::<(), _>("Stop", ()); }

    #[allow(dead_code)]
    pub fn next(&self) { self.send_call_simple::<(), _>("Next", ()); }

    #[allow(dead_code)]
    pub fn previous(&self) { self.send_call_simple::<(), _>("Previous", ()); }

    pub fn state(&self) -> MutexGuard<'_, PlayerState> {
        return self.state.lock().unwrap();
    }
}
