use std::cell::RefCell;
use std::error::Error;
use std::fmt::Debug;
use std::sync::Once;

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::subscriber::Interest;
use tracing::{Event, Metadata, Subscriber};

pub(crate) type LoggedEvent = Vec<(String, String)>;

thread_local! {
    static CAPTURED: RefCell<Option<Vec<LoggedEvent>>> = const { RefCell::new(None) };
}

struct CaptureOnMarkedThreads;

#[derive(Default)]
struct Fields(LoggedEvent);

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        self.0.push((field.name().to_owned(), format!("{value:?}")));
    }

    fn record_error(&mut self, field: &Field, value: &(dyn Error + 'static)) {
        self.0
            .push((field.name().to_owned(), format!("error field: {value}")));
    }
}

impl Subscriber for CaptureOnMarkedThreads {
    fn register_callsite(&self, _: &'static Metadata<'static>) -> Interest {
        Interest::sometimes()
    }

    fn enabled(&self, _: &Metadata<'_>) -> bool {
        CAPTURED.with_borrow(Option::is_some)
    }

    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        fields
            .0
            .push(("level".to_owned(), event.metadata().level().to_string()));
        fields.0.sort();
        CAPTURED.with_borrow_mut(|captured| {
            if let Some(events) = captured {
                events.push(fields.0);
            }
        });
    }

    fn enter(&self, _: &Id) {}

    fn exit(&self, _: &Id) {}
}

pub(crate) fn captured(work: impl FnOnce()) -> Vec<LoggedEvent> {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        tracing::subscriber::set_global_default(CaptureOnMarkedThreads)
            .expect("no other global subscriber in the unit tests");
    });
    tracing::callsite::rebuild_interest_cache();
    CAPTURED.set(Some(Vec::new()));
    work();
    CAPTURED.take().unwrap_or_default()
}

pub(crate) fn logged(fields: &[(&str, &str)]) -> LoggedEvent {
    fields
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect()
}
