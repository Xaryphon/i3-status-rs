use std::{sync::{Arc, Mutex, MutexGuard}, time::Duration};

use dbus::{nonblock::{Proxy, SyncConnection, MethodReply, MsgMatch}, arg::{ReadAll, AppendAll, self, RefArg}, message::MatchRule};
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

    fn extract_value_from_variant(variant: &dyn RefArg) -> Option<&dyn RefArg> {
        return variant.as_iter()?.next();
    }

    fn update_metadata(&mut self, metadata: Box<dyn RefArg>) {
        let mut iter = match metadata.as_iter() {
            Some(x) => x,
            None => return
        };
        loop {
            let key = match iter.next() {
                Some(value) => match value.as_str() {
                    Some(x) => x,
                    None => continue
                },
                None => break,
            };
            let value = match iter.next().and_then(Self::extract_value_from_variant) {
                Some(value) => value,
                None => break // invalid
            };

            match key {
                "xesam:title" => {
                    self.title = value.as_str().unwrap_or("").to_string();
                },
                "xesam:artist" => {
                    self.artists.clear();
                    let iter = match value.as_iter() {
                        Some(iter) => iter,
                        None => continue
                    };
                    for artist in iter {
                        self.artists.push(match artist.as_str() {
                            Some(value) => value,
                            None => continue
                        }.to_string());
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
                    self.playing = value.as_str()
                        .and_then(|value| Some(value == "Playing"))
                        .unwrap_or(false);
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
    async fn create_property_changed_handler(conn: Arc<SyncConnection>, bus_name: String, state: Arc<Mutex<PlayerState>>) -> Result<MsgMatch, dbus::Error> {
        let rule: MatchRule<'_> = MatchRule::new_signal("org.freedesktop.DBus.Properties", "PropertiesChanged")
                .with_sender(bus_name.clone());
        let r#match = conn.add_match(rule);
        return r#match.await
            .and_then(|x| Ok(x.cb(move |_, (interface_name, changed_properties, invalidated_properties,): (String, arg::PropMap, Vec<String>)| {
                if interface_name != INTERFACE {
                    return true;
                }

                if invalidated_properties.len() > 0 {
                    eprintln!("Unhandled PropertyChanged invalidated_properties.len() > 0");
                }

                state.lock().unwrap().update(changed_properties);
                true
        })));
    }

    async fn create_name_owner_changed_handler(conn: Arc<SyncConnection>, bus_name: String, proxy: Proxy<'static, Arc<SyncConnection>>, state: Arc<Mutex<PlayerState>>) -> Result<MsgMatch, dbus::Error> {
        let rule = MatchRule::new_signal("org.freedesktop.DBus", "NameOwnerChanged")
            .with_sender("org.freedesktop.DBus");
        let r#match = conn.add_match(rule);
        return r#match.await
            .and_then(|x| Ok(x.cb(move |_, (name, _old_owner, new_owner): (String, String, String)| {
                if name != bus_name {
                    return true;
                }

                if new_owner.is_empty() {
                    state.lock().unwrap().name_lost();
                } else {
                    let proxy2 = proxy.clone();
                    let state2 = state.clone();
                    tokio::spawn(async move {
                        // Spotify on startup may take some time to get the song information and
                        // won't signal when it has them. So we wait a bit and ask for them manually.
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        let (metadata,): (arg::Variant<Box<dyn RefArg + 'static>>,) = match proxy2.method_call("org.freedesktop.DBus.Properties", "Get", (INTERFACE, "Metadata")).await {
                            Ok(x) => x,
                            Err(err) => {
                                eprintln!("Failed to get metadata: {}", err);
                                return;
                            }
                        };
                        state2.lock().unwrap().update_metadata(metadata.0);
                    });
                }
                true
            })));
    }

    fn create_watcher(conn: Arc<SyncConnection>, bus_name: String, proxy: Proxy<'static, Arc<SyncConnection>>, destruct: Arc<Notify>, state: Arc<Mutex<PlayerState>>) -> tokio::task::JoinHandle<()> {
        return tokio::spawn(async move {
            let signal_property_changed = match Self::create_property_changed_handler(conn.clone(), bus_name.clone(), state.clone()).await {
                Ok(handler) => handler,
                Err(err) => {
                    eprintln!("Failed to AddMatch on PropertiesChanged: {}", err);
                    return;
                }
            };

            let signal_name_owner_changed = match Self::create_name_owner_changed_handler(conn.clone(), bus_name, proxy.clone(), state.clone()).await {
                Ok(handler) => handler,
                Err(err) => {
                    // There must be a more elegant solution for this, maybe something like defer?
                    let _ = conn.remove_match(signal_property_changed.token()).await;
                    eprintln!("Failed to AddMatch on NameOwnerChanged: {}", err);
                    return;
                }
            };

            let props: Option<arg::PropMap> = match proxy.method_call("org.freedesktop.DBus.Properties", "GetAll", (INTERFACE,)).await {
                Ok((x,)) => Some(x),
                Err(err) => {
                    eprintln!("Failed to get properties: {}", err);
                    None
                }
            };

            match props {
                Some(props) => state.lock().unwrap().update(props),
                None => {}
            };

            destruct.notified().await;

            let _ = conn.remove_match(signal_name_owner_changed.token()).await;
            let _ = conn.remove_match(signal_property_changed.token()).await;
        });
    }

    pub fn new(conn: Arc<SyncConnection>, instance: &str, invalidate: Arc<Notify>) -> Self {
        let bus_name = format!("org.mpris.MediaPlayer2.{}", instance);
        let proxy = Proxy::new(
            bus_name.clone(),
            "/org/mpris/MediaPlayer2",
            Duration::from_secs(5),
            conn.clone());

        let destruct = Arc::new(Notify::new());
        let state = Arc::new(Mutex::new(PlayerState::new(invalidate)));

        Self::create_watcher(conn, bus_name, proxy.clone(), destruct.clone(), state.clone());

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
