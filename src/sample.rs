//! Stand-ins the screenshot script shows, to look at dialogs without a
//! game that brings them up.

use crate::model::{Prompt, PromptOrigin};
use crate::playable::UploadDetail;
use crate::report::{Rating, Report, Run};
use crate::ui::ReportView;

/// The notice step's message.
pub const NOTICE: &str = "Couldn't check for updates: a sample failure from the screenshot script";

/// A license to accept, or with a count, a pick between that many
/// downloads.
pub fn prompt(choice: Option<usize>) -> Prompt {
    match choice {
        Some(count) => Prompt {
            id: 0,
            origin: PromptOrigin::Sample,
            title: "Which download?".into(),
            body: format!("Sample has {count} downloads for this device."),
            choices: (1..=count)
                .map(|i| format!("Sample - Linux - build {i}"))
                .collect(),
            focus: 0,
            primary: None,
            stacked: true,
            details: (1..=count)
                .map(|i| UploadDetail {
                    platforms: vec![("Linux ARM64".into(), i == 1), ("Linux x64".into(), false)],
                    notes: vec!["135.9 MB".into()],
                })
                .collect(),
            progress: None,
        },
        None => Prompt {
            id: 0,
            origin: PromptOrigin::Sample,
            title: "License agreement".into(),
            body: "This is a sample license shown by the screenshot script. ".repeat(12),
            choices: vec!["Accept".into(), "Decline".into()],
            focus: 0,
            primary: Some(0),
            stacked: false,
            details: Vec::new(),
            progress: None,
        },
    }
}

/// The compatibility report form for a game that is never sent.
pub fn report() -> ReportView {
    let draft = Report {
        game_id: 0,
        upload_id: 0,
        build_id: None,
        rating: Rating::Perfect,
        flags: Vec::new(),
        run: Run::default(),
    };
    ReportView {
        sample: true,
        ..ReportView::new("Sample game".into(), draft)
    }
}
