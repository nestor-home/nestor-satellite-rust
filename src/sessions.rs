//! The sessions a person opens in Nestor to watch through the device's camera or call through it,
//! and the still pictures they ask for. The rules of `nestor.satellite/1` are kept here, whatever the
//! driver does:
//!
//! - M1. A connection only for an `intent.media.open` received on the link, and only for what the
//!   device was given and `welcome` allowed: video with a camera and `video.capture`, a call with a
//!   microphone, a speaker and both `audio.capture` and `audio.playback`. A session asking for more
//!   is refused whole: acknowledged, logged, nothing offered.
//! - M2. It ends on `intent.media.close`, at `max_s` by the device's own clock, and when the link
//!   closes.
//! - M3. The [`MediaIndicator`] is told `true` before anything is sent, `false` once nothing is
//!   (sessions and still pictures).
//! - M5. During a call, the voice with the assistant steps aside: it hangs up before the call
//!   connects, and is offered again after.
//! - M6. A still picture is sent as `media.snapshot`, `of` the intent that asked for it.
//!
//! M4 holds by construction: the device only connects out to the core, it listens on no port. The
//! connection itself (WebRTC, feature `voice`; the camera, feature `video`) is given as a function,
//! so these rules hold, and are tested, in every build.

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use futures_util::future::BoxFuture;
use reqwest::Client;
use tokio::sync::{mpsc, watch};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::Instant;
use tracing::{debug, info, warn};

use crate::body::Body;
use crate::client::Backoff;
use crate::error::Result;
use crate::i18n::Language;
use crate::wire::{self, MediaIntent, MediaOpen};

/// Shows that something is filmed or heard in a session, or a picture taken (a light, a screen): the
/// satellite declares `media.indicator`. Called from the network task: return at once. A closure
/// `Fn(bool)` is an indicator.
pub trait MediaIndicator: Send + Sync + 'static {
    /// `true` before anything is sent, `false` once nothing is.
    fn media(&self, active: bool);
}

impl<F: Fn(bool) + Send + Sync + 'static> MediaIndicator for F {
    fn media(&self, active: bool) {
        self(active);
    }
}

/// Carries one session until its connection ends or `Stop` says it must (returns then, its devices
/// released).
pub(crate) type Connect = Arc<dyn Fn(MediaOpen, Stop) -> BoxFuture<'static, Result<()>> + Send + Sync>;
/// A still picture, as JPEG.
pub(crate) type Still = Arc<dyn Fn() -> BoxFuture<'static, Result<Vec<u8>>> + Send + Sync>;
/// The voice with the assistant, until it ends.
pub(crate) type Hear = Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>;

/// When a session must end: the core closed it, the link closed, or its time is up.
#[cfg_attr(not(feature = "voice"), allow(dead_code))] // read by the connection, which needs WebRTC
pub(crate) struct Stop {
    told: watch::Receiver<bool>,
    deadline: Instant,
}

#[cfg_attr(not(feature = "voice"), allow(dead_code))]
impl Stop {
    /// Until the session must end.
    pub(crate) async fn wait(&mut self) {
        tokio::select! {
            _ = self.told.wait_for(|told| *told) => {}
            () = tokio::time::sleep_until(self.deadline) => {}
        }
    }
}

/// The voice of one link with the assistant, which steps aside during a call: `hold` hangs it up,
/// `start` offers it again; after `end` (the link closed) it never starts.
pub(crate) struct VoiceHold {
    hear: Option<Hear>,
    state: StdMutex<(Option<JoinHandle<()>>, bool)>, // the running voice, ended
}

impl VoiceHold {
    /// A voice offered by `hear`, or none.
    pub(crate) fn new(hear: Option<Hear>) -> Self {
        Self { hear, state: StdMutex::new((None, false)) }
    }

    pub(crate) fn start(&self) {
        let Some(hear) = &self.hear else { return };
        if let Ok(mut state) = self.state.lock()
            && state.0.is_none()
            && !state.1
        {
            state.0 = Some(tokio::spawn(hear()));
        }
    }

    pub(crate) async fn hold(&self) {
        let running = self.state.lock().ok().and_then(|mut state| state.0.take());
        if let Some(task) = running {
            task.abort();
            let _ = task.await;
            debug!("the voice is hung up for a call");
        }
    }

    pub(crate) async fn end(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.1 = true;
        }
        self.hold().await;
    }
}

/// What a link gives its voice and its sessions to connect with.
#[derive(Clone)]
#[cfg_attr(not(feature = "voice"), allow(dead_code))] // nothing connects without WebRTC
pub(crate) struct MediaLink {
    pub(crate) http: Client,
    /// The core's base URL.
    pub(crate) core: String,
    pub(crate) token: String,
    pub(crate) device_id: String,
    pub(crate) body: Arc<dyn Body>,
    /// The voice's delay between offers.
    pub(crate) backoff: Backoff,
}

/// What one link can do with media, from what the satellite was given and `welcome` allowed.
pub(crate) struct Means {
    /// A camera given, `video.capture` allowed.
    pub(crate) films: bool,
    /// A microphone and a speaker given, `audio.capture` and `audio.playback` allowed.
    pub(crate) calls: bool,
    pub(crate) connect: Option<Connect>,
    pub(crate) still: Option<Still>,
}

/// The sessions of one link.
pub(crate) struct MediaSessions {
    inner: Arc<Inner>,
    tasks: StdMutex<JoinSet<()>>,
}

struct Inner {
    means: Means,
    reply: mpsc::UnboundedSender<String>,
    voice: Arc<VoiceHold>,
    indicator: Option<Arc<dyn MediaIndicator>>,
    language: Language,
    state: StdMutex<State>,
}

#[derive(Default)]
struct State {
    /// The sessions under way, by id: what tells each to stop.
    live: HashMap<String, watch::Sender<bool>>,
    /// Still pictures being taken.
    pictures: usize,
    calling: bool,
    /// What the indicator shows.
    shown: bool,
}

impl MediaSessions {
    pub(crate) fn new(
        means: Means,
        reply: mpsc::UnboundedSender<String>,
        voice: Arc<VoiceHold>,
        indicator: Option<Arc<dyn MediaIndicator>>,
        language: Language,
    ) -> Self {
        let state = StdMutex::new(State::default());
        let inner = Arc::new(Inner { means, reply, voice, indicator, language, state });
        Self { inner, tasks: StdMutex::new(JoinSet::new()) }
    }

    /// A media intent of the link (`of`, its id): open or end a session, or take a picture. Returns
    /// at once.
    pub(crate) fn handle(&self, of: String, media: MediaIntent) {
        match media {
            MediaIntent::Open(opening) => self.open(opening),
            MediaIntent::Close { session } => {
                if let Some(stop) = self.inner.state().live.get(&session) {
                    stop.send_replace(true);
                }
            }
            MediaIntent::Snapshot => self.snapshot(of),
        }
    }

    /// The link closed: every session ends (M2), and this waits until they did.
    pub(crate) async fn close(&self) {
        for stop in self.inner.state().live.values() {
            stop.send_replace(true);
        }
        let mut tasks = match self.tasks.lock() {
            Ok(mut tasks) => std::mem::take(&mut *tasks),
            Err(_) => return,
        };
        while tasks.join_next().await.is_some() {}
    }

    fn open(&self, opening: MediaOpen) {
        let inner = &self.inner;
        let means = &inner.means;
        let (stop, told) = watch::channel(false);
        {
            let mut state = inner.state();
            if state.live.contains_key(&opening.session) {
                return;
            }
            let wanted = opening.video || opening.audio;
            let refused = !wanted
                || (opening.video && !means.films)
                || (opening.audio && (!means.calls || state.calling))
                || means.connect.is_none();
            if refused {
                warn!(session = %opening.session, "{}", inner.language.say("media-refused", &[]));
                return;
            }
            state.calling |= opening.audio;
            state.live.insert(opening.session.clone(), stop);
        }
        inner.indicate();
        // From the moment it was asked, by the device's own clock.
        let deadline = Instant::now() + Duration::from_secs(u64::from(opening.max_s));
        let inner = inner.clone();
        self.spawn(async move {
            if opening.audio {
                inner.voice.hold().await;
            }
            let Some(connect) = inner.means.connect.clone() else { return };
            info!(session = %opening.session, video = opening.video, call = opening.audio, "media session opened");
            match connect(opening.clone(), Stop { told, deadline }).await {
                Ok(()) if Instant::now() >= deadline => {
                    let seconds = i64::from(opening.max_s).into();
                    info!("{}", inner.language.say("media-timed-out", &[("seconds", seconds)]));
                }
                Ok(()) => info!(session = %opening.session, "media session ended"),
                Err(error) => warn!("{}", inner.language.say("media-failed", &[("error", error.to_string().into())])),
            }
            inner.ended(&opening);
        });
    }

    fn snapshot(&self, of: String) {
        let inner = &self.inner;
        let Some(still) = inner.means.still.clone().filter(|_| inner.means.films) else {
            warn!("{}", inner.language.say("media-refused", &[]));
            return;
        };
        inner.state().pictures += 1;
        inner.indicate();
        let inner = inner.clone();
        self.spawn(async move {
            let sent = match still().await {
                Ok(jpeg) => wire::snapshot(&of, &jpeg).map_err(Into::into),
                Err(error) => Err(error),
            };
            match sent {
                Ok(message) => {
                    let _ = inner.reply.send(message.to_string());
                }
                Err(error) => warn!("{}", inner.language.say("media-failed", &[("error", error.to_string().into())])),
            }
            inner.state().pictures -= 1;
            inner.indicate();
        });
    }

    fn spawn(&self, work: impl Future<Output = ()> + Send + 'static) {
        if let Ok(mut tasks) = self.tasks.lock() {
            while tasks.try_join_next().is_some() {}
            tasks.spawn(work);
        }
    }
}

impl Drop for MediaSessions {
    /// The link's task was dropped (the satellite stopped): its sessions are aborted with it, and the
    /// indicator goes off.
    fn drop(&mut self) {
        if let Ok(mut tasks) = self.tasks.lock() {
            tasks.abort_all();
        }
        let shown = std::mem::take(&mut self.inner.state().shown);
        if let (true, Some(indicator)) = (shown, &self.inner.indicator) {
            indicator.media(false);
        }
    }
}

impl Inner {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn ended(&self, opening: &MediaOpen) {
        {
            let mut state = self.state();
            state.live.remove(&opening.session);
            if opening.audio {
                state.calling = false;
            }
        }
        if opening.audio {
            self.voice.start();
        }
        self.indicate();
    }

    /// Tell the indicator when something starts or stops being filmed or heard.
    fn indicate(&self) {
        let Some(indicator) = &self.indicator else { return };
        let active = {
            let mut state = self.state();
            let active = !state.live.is_empty() || state.pictures > 0;
            if active == state.shown {
                return;
            }
            state.shown = active;
            active
        };
        indicator.media(active);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::Value;

    use super::*;

    /// What the fake connection saw: sessions connected and when they ended, the indicator's states
    /// at each connection, the voice's offers.
    #[derive(Default)]
    struct Seen {
        connected: StdMutex<Vec<String>>,
        ended: StdMutex<Vec<String>>,
        shown: StdMutex<Vec<bool>>,
        offers: AtomicUsize,
        hangups: Arc<AtomicUsize>,
    }

    /// Counts the voice hung up: dropped with the voice's task.
    struct HangUp(Arc<AtomicUsize>);

    impl Drop for HangUp {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl Seen {
        fn connected(&self) -> Vec<String> {
            self.connected.lock().unwrap().clone()
        }
        fn ended(&self) -> Vec<String> {
            self.ended.lock().unwrap().clone()
        }
        fn shown(&self) -> Vec<bool> {
            self.shown.lock().unwrap().clone()
        }
    }

    struct Link {
        seen: Arc<Seen>,
        sessions: MediaSessions,
        voice: Arc<VoiceHold>,
        replies: mpsc::UnboundedReceiver<String>,
    }

    /// A link that `films` and `calls` (given and allowed), whose connections last until stopped,
    /// with a voice that counts its offers.
    fn link(films: bool, calls: bool) -> Link {
        let seen = Arc::new(Seen::default());
        let (on_connect, on_end, on_offer, on_show) = (seen.clone(), seen.clone(), seen.clone(), seen.clone());
        let connect: Connect = Arc::new(move |opening: MediaOpen, mut stop: Stop| {
            let (connected, ended) = (on_connect.clone(), on_end.clone());
            Box::pin(async move {
                let shown = connected.shown().last() == Some(&true);
                let hung = connected.hangups.load(Ordering::SeqCst);
                connected.connected.lock().unwrap().push(format!("{} on:{shown} hung:{hung}", opening.session));
                stop.wait().await;
                ended.ended.lock().unwrap().push(opening.session);
                Ok(())
            })
        });
        let still: Still = Arc::new(|| Box::pin(async { Ok(vec![0xff, 0xd8, 0xff, 0xd9]) }));
        let hear: Hear = Arc::new(move || {
            on_offer.offers.fetch_add(1, Ordering::SeqCst);
            let hangup = HangUp(on_offer.hangups.clone());
            Box::pin(async move {
                let _hangup = hangup;
                std::future::pending::<()>().await;
            })
        });
        let voice = Arc::new(VoiceHold::new(Some(hear)));
        voice.start();
        let (reply, replies) = mpsc::unbounded_channel();
        let indicator = move |active: bool| on_show.shown.lock().unwrap().push(active);
        let means = Means { films, calls, connect: Some(connect), still: Some(still) };
        let sessions = MediaSessions::new(means, reply, voice.clone(), Some(Arc::new(indicator)), Language::En);
        Link { seen, sessions, voice, replies }
    }

    fn open(session: &str, video: bool, audio: bool, max_s: u32) -> MediaIntent {
        MediaIntent::Open(MediaOpen { session: session.into(), video, audio, max_s })
    }

    fn close(session: &str) -> MediaIntent {
        MediaIntent::Close { session: session.into() }
    }

    async fn settle() {
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn a_session_asking_more_than_what_was_given_and_allowed_is_never_connected() {
        for (films, calls, video, audio) in [
            (false, true, true, false), // filming, without a camera or not allowed
            (true, false, false, true), // a call, without microphone and speaker or not allowed
            (false, true, true, true),  // a call with video, without a camera
            (true, true, false, false), // nothing asked
        ] {
            let link = link(films, calls);
            link.sessions.handle("i".into(), open("s", video, audio, 60));
            link.sessions.handle("i".into(), MediaIntent::Snapshot);
            settle().await;
            assert_eq!(link.seen.connected(), Vec::<String>::new(), "{films} {calls} {video} {audio}");
            if !films {
                assert_eq!(link.seen.shown(), Vec::<bool>::new(), "nothing shown: nothing filmed nor heard");
            }
        }
    }

    #[tokio::test]
    async fn one_call_at_a_time() {
        let link = link(true, true);
        link.sessions.handle("i".into(), open("first", false, true, 60));
        link.sessions.handle("i".into(), open("second", true, true, 60));
        link.sessions.handle("i".into(), open("watch", true, false, 60));
        settle().await;
        let mut connected: Vec<String> =
            link.seen.connected().iter().map(|c| c.split(' ').next().unwrap().to_owned()).collect();
        connected.sort();
        assert_eq!(connected, ["first", "watch"], "a second call is refused, watching meanwhile is not");
    }

    #[tokio::test(start_paused = true)]
    async fn a_session_ends_when_told_when_its_time_is_up_and_when_the_link_closes() {
        let link = link(true, false);
        link.sessions.handle("i".into(), open("told", true, false, 60));
        link.sessions.handle("i".into(), open("timed", true, false, 2));
        link.sessions.handle("i".into(), open("linked", true, false, 60));
        settle().await;
        link.sessions.handle("i".into(), close("told"));
        link.sessions.handle("i".into(), close("unknown"));
        settle().await;
        assert_eq!(link.seen.ended(), ["told"]);
        tokio::time::sleep(Duration::from_millis(1_999)).await;
        assert_eq!(link.seen.ended(), ["told"], "not before its time");
        tokio::time::sleep(Duration::from_millis(2)).await;
        settle().await;
        assert_eq!(link.seen.ended(), ["told", "timed"]);
        let closing = Instant::now();
        link.sessions.close().await;
        assert_eq!(link.seen.ended(), ["told", "timed", "linked"]);
        assert!(closing.elapsed() < Duration::from_secs(1), "at once, not at its time limit");
        assert_eq!(link.seen.shown(), [true, false], "on before the first connected, off after the last");
    }

    #[tokio::test]
    async fn the_voice_hangs_up_for_a_call_and_is_offered_again_after_it() {
        let link = link(false, true);
        settle().await;
        assert_eq!(link.seen.offers.load(Ordering::SeqCst), 1);
        link.sessions.handle("i".into(), open("call", false, true, 60));
        settle().await;
        assert_eq!(link.seen.connected(), ["call on:true hung:1"], "hung up before the call connects");
        link.sessions.handle("i".into(), close("call"));
        settle().await;
        assert_eq!(link.seen.offers.load(Ordering::SeqCst), 2, "offered again after");
        link.voice.end().await;
        link.sessions.handle("i".into(), open("late", false, true, 60));
        link.sessions.handle("i".into(), close("late"));
        settle().await;
        assert_eq!(link.seen.offers.load(Ordering::SeqCst), 2, "never after the link closed");
    }

    #[tokio::test]
    async fn a_still_picture_answers_the_intent_that_asked_for_it_and_is_shown_while_taken() {
        let mut link = link(true, false);
        link.sessions.handle("intent-9".into(), MediaIntent::Snapshot);
        let sent: Value = serde_json::from_str(&link.replies.recv().await.unwrap()).unwrap();
        assert_eq!((sent["type"].as_str(), &sent["payload"]["of"]), (Some("media.snapshot"), &"intent-9".into()));
        assert_eq!(sent["payload"]["jpeg"], "/9j/2Q==");
        settle().await;
        assert_eq!(link.seen.shown(), [true, false]);
    }
}
