use gpui::{App, Window, actions};

use ranki_model::Rating;

use crate::ThemeChoice;

actions!(
    ranki_ui,
    [
        Reveal,
        Space,
        GradeAgain,
        GradeHard,
        GradeGood,
        GradeEasy,
        Retry,
        RetryStatistics,
        Reload,
        Undo,
        NextField,
        PreviousField,
        StudyAgain,
        EndSession,
        OpenDecks,
        OpenStatistics,
        OpenBrowse,
        OpenAdd,
        CreateDeck,
        OpenSettings,
        UseLightTheme,
        UseDarkTheme,
        UseSystemTheme,
        UseReducedMotion,
        UseFullMotion,
        UseSystemMotion,
        AcceptTagSuggestion,
        DismissTagSuggestions,
        SubmitDeck,
        SaveNote,
        CancelAdd,
        AddOcclusionMask,
        RemoveOcclusionMask,
        ApplyCustomColor,
        ToggleHelp,
        ConfirmDialog,
        CancelDialog,
        NextDeck,
        ClearSearch,
        FocusDeckSearch,
        ClearBrowseSearch,
        BrowseUp,
        BrowseDown,
        SyncSignIn,
        SyncSignOut,
        SyncNow,
        ToggleAutoSync,
        SyncKeepLocal,
        SyncKeepRemote,
    ]
);

/// UI action routed through the single dispatcher.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiAction {
    Reveal,
    Space,
    Grade(Rating),
    Retry,
    RetryStatistics,
    Reload,
    Undo,
    StudyAgain,
    EndSession,
    OpenDecks,
    OpenStatistics,
    OpenBrowse,
    OpenAdd,
    OpenSettings,
    SetTheme(ThemeChoice),
    SetReduceMotion(Option<bool>),
    CreateDeck,
    SubmitDeck,
    SaveNote,
    CancelAdd,
    ToggleHelp,
    Confirm,
    CancelDialog,
    NextDeck,
    ClearSearch,
    FocusDeckSearch,
    ClearBrowseSearch,
    BrowseUp,
    BrowseDown,
    SyncSignIn,
    SyncSignOut,
    SyncNow,
    ToggleAutoSync,
    SyncKeepLocal,
    SyncKeepRemote,
}

impl From<&Reveal> for UiAction {
    fn from(_: &Reveal) -> Self {
        Self::Reveal
    }
}
impl From<&Space> for UiAction {
    fn from(_: &Space) -> Self {
        Self::Space
    }
}
impl From<&GradeAgain> for UiAction {
    fn from(_: &GradeAgain) -> Self {
        Self::Grade(Rating::Again)
    }
}
impl From<&GradeHard> for UiAction {
    fn from(_: &GradeHard) -> Self {
        Self::Grade(Rating::Hard)
    }
}
impl From<&GradeGood> for UiAction {
    fn from(_: &GradeGood) -> Self {
        Self::Grade(Rating::Good)
    }
}
impl From<&GradeEasy> for UiAction {
    fn from(_: &GradeEasy) -> Self {
        Self::Grade(Rating::Easy)
    }
}
impl From<&Retry> for UiAction {
    fn from(_: &Retry) -> Self {
        Self::Retry
    }
}
impl From<&RetryStatistics> for UiAction {
    fn from(_: &RetryStatistics) -> Self {
        Self::RetryStatistics
    }
}
impl From<&Reload> for UiAction {
    fn from(_: &Reload) -> Self {
        Self::Reload
    }
}
impl From<&Undo> for UiAction {
    fn from(_: &Undo) -> Self {
        Self::Undo
    }
}
impl From<&StudyAgain> for UiAction {
    fn from(_: &StudyAgain) -> Self {
        Self::StudyAgain
    }
}
impl From<&EndSession> for UiAction {
    fn from(_: &EndSession) -> Self {
        Self::EndSession
    }
}
impl From<&OpenDecks> for UiAction {
    fn from(_: &OpenDecks) -> Self {
        Self::OpenDecks
    }
}
impl From<&OpenSettings> for UiAction {
    fn from(_: &OpenSettings) -> Self {
        Self::OpenSettings
    }
}
impl From<&UseLightTheme> for UiAction {
    fn from(_: &UseLightTheme) -> Self {
        Self::SetTheme(ThemeChoice::Light)
    }
}
impl From<&UseDarkTheme> for UiAction {
    fn from(_: &UseDarkTheme) -> Self {
        Self::SetTheme(ThemeChoice::Dark)
    }
}
impl From<&UseSystemTheme> for UiAction {
    fn from(_: &UseSystemTheme) -> Self {
        Self::SetTheme(ThemeChoice::System)
    }
}
impl From<&UseReducedMotion> for UiAction {
    fn from(_: &UseReducedMotion) -> Self {
        Self::SetReduceMotion(Some(true))
    }
}
impl From<&UseFullMotion> for UiAction {
    fn from(_: &UseFullMotion) -> Self {
        Self::SetReduceMotion(Some(false))
    }
}
impl From<&UseSystemMotion> for UiAction {
    fn from(_: &UseSystemMotion) -> Self {
        Self::SetReduceMotion(None)
    }
}
impl From<&OpenStatistics> for UiAction {
    fn from(_: &OpenStatistics) -> Self {
        Self::OpenStatistics
    }
}
impl From<&OpenAdd> for UiAction {
    fn from(_: &OpenAdd) -> Self {
        Self::OpenAdd
    }
}
impl From<&CreateDeck> for UiAction {
    fn from(_: &CreateDeck) -> Self {
        Self::CreateDeck
    }
}
impl From<&SubmitDeck> for UiAction {
    fn from(_: &SubmitDeck) -> Self {
        Self::SubmitDeck
    }
}
impl From<&SaveNote> for UiAction {
    fn from(_: &SaveNote) -> Self {
        Self::SaveNote
    }
}
impl From<&CancelAdd> for UiAction {
    fn from(_: &CancelAdd) -> Self {
        Self::CancelAdd
    }
}
impl From<&ToggleHelp> for UiAction {
    fn from(_: &ToggleHelp) -> Self {
        Self::ToggleHelp
    }
}
impl From<&ConfirmDialog> for UiAction {
    fn from(_: &ConfirmDialog) -> Self {
        Self::Confirm
    }
}
impl From<&CancelDialog> for UiAction {
    fn from(_: &CancelDialog) -> Self {
        Self::CancelDialog
    }
}
impl From<&NextDeck> for UiAction {
    fn from(_: &NextDeck) -> Self {
        Self::NextDeck
    }
}
impl From<&ClearSearch> for UiAction {
    fn from(_: &ClearSearch) -> Self {
        Self::ClearSearch
    }
}
impl From<&FocusDeckSearch> for UiAction {
    fn from(_: &FocusDeckSearch) -> Self {
        Self::FocusDeckSearch
    }
}
impl From<&OpenBrowse> for UiAction {
    fn from(_: &OpenBrowse) -> Self {
        Self::OpenBrowse
    }
}

impl From<&ClearBrowseSearch> for UiAction {
    fn from(_: &ClearBrowseSearch) -> Self {
        Self::ClearBrowseSearch
    }
}
impl From<&BrowseUp> for UiAction {
    fn from(_: &BrowseUp) -> Self {
        Self::BrowseUp
    }
}
impl From<&BrowseDown> for UiAction {
    fn from(_: &BrowseDown) -> Self {
        Self::BrowseDown
    }
}
/// Dispatches one of the app's GPUI actions from a pointer handler.
pub fn dispatch_app_action(
    window: &mut Window,
    cx: &mut App,
    action: UiAction,
) {
    match action {
        UiAction::Reveal => window.dispatch_action(Box::new(Reveal), cx),
        UiAction::Space => window.dispatch_action(Box::new(Space), cx),
        UiAction::Grade(Rating::Again) => {
            window.dispatch_action(Box::new(GradeAgain), cx);
        }
        UiAction::Grade(Rating::Hard) => {
            window.dispatch_action(Box::new(GradeHard), cx);
        }
        UiAction::Grade(Rating::Good) => {
            window.dispatch_action(Box::new(GradeGood), cx);
        }
        UiAction::Grade(Rating::Easy) => {
            window.dispatch_action(Box::new(GradeEasy), cx);
        }
        UiAction::Retry => window.dispatch_action(Box::new(Retry), cx),
        UiAction::RetryStatistics => {
            window.dispatch_action(Box::new(RetryStatistics), cx);
        }
        UiAction::Reload => window.dispatch_action(Box::new(Reload), cx),
        UiAction::Undo => window.dispatch_action(Box::new(Undo), cx),
        UiAction::StudyAgain => {
            window.dispatch_action(Box::new(StudyAgain), cx);
        }
        UiAction::EndSession => {
            window.dispatch_action(Box::new(EndSession), cx);
        }
        UiAction::OpenDecks => window.dispatch_action(Box::new(OpenDecks), cx),
        UiAction::OpenStatistics => {
            window.dispatch_action(Box::new(OpenStatistics), cx);
        }
        UiAction::OpenBrowse => {
            window.dispatch_action(Box::new(OpenBrowse), cx);
        }
        UiAction::OpenAdd => window.dispatch_action(Box::new(OpenAdd), cx),
        UiAction::OpenSettings => {
            window.dispatch_action(Box::new(OpenSettings), cx);
        }
        UiAction::SetTheme(ThemeChoice::Light) => {
            window.dispatch_action(Box::new(UseLightTheme), cx);
        }
        UiAction::SetTheme(ThemeChoice::Dark) => {
            window.dispatch_action(Box::new(UseDarkTheme), cx);
        }
        UiAction::SetTheme(ThemeChoice::System) => {
            window.dispatch_action(Box::new(UseSystemTheme), cx);
        }
        UiAction::SetReduceMotion(Some(true)) => {
            window.dispatch_action(Box::new(UseReducedMotion), cx);
        }
        UiAction::SetReduceMotion(Some(false)) => {
            window.dispatch_action(Box::new(UseFullMotion), cx);
        }
        UiAction::SetReduceMotion(None) => {
            window.dispatch_action(Box::new(UseSystemMotion), cx);
        }
        UiAction::CreateDeck => {
            window.dispatch_action(Box::new(CreateDeck), cx);
        }
        UiAction::SubmitDeck => {
            window.dispatch_action(Box::new(SubmitDeck), cx);
        }
        UiAction::SaveNote => window.dispatch_action(Box::new(SaveNote), cx),
        UiAction::CancelAdd => window.dispatch_action(Box::new(CancelAdd), cx),
        UiAction::ToggleHelp => {
            window.dispatch_action(Box::new(ToggleHelp), cx);
        }
        UiAction::Confirm => {
            window.dispatch_action(Box::new(ConfirmDialog), cx);
        }
        UiAction::CancelDialog => {
            window.dispatch_action(Box::new(CancelDialog), cx);
        }
        UiAction::NextDeck => window.dispatch_action(Box::new(NextDeck), cx),
        UiAction::ClearSearch => {
            window.dispatch_action(Box::new(ClearSearch), cx);
        }
        UiAction::FocusDeckSearch => {
            window.dispatch_action(Box::new(FocusDeckSearch), cx);
        }
        UiAction::ClearBrowseSearch => {
            window.dispatch_action(Box::new(ClearBrowseSearch), cx);
        }
        UiAction::BrowseUp => window.dispatch_action(Box::new(BrowseUp), cx),
        UiAction::BrowseDown => {
            window.dispatch_action(Box::new(BrowseDown), cx);
        }
        UiAction::SyncSignIn => {
            window.dispatch_action(Box::new(SyncSignIn), cx);
        }
        UiAction::SyncSignOut => {
            window.dispatch_action(Box::new(SyncSignOut), cx);
        }
        UiAction::SyncNow => {
            window.dispatch_action(Box::new(SyncNow), cx);
        }
        UiAction::ToggleAutoSync => {
            window.dispatch_action(Box::new(ToggleAutoSync), cx);
        }
        UiAction::SyncKeepLocal => {
            window.dispatch_action(Box::new(SyncKeepLocal), cx);
        }
        UiAction::SyncKeepRemote => {
            window.dispatch_action(Box::new(SyncKeepRemote), cx);
        }
    }
}

impl From<&SyncNow> for UiAction {
    fn from(_: &SyncNow) -> Self {
        Self::SyncNow
    }
}

impl From<&ToggleAutoSync> for UiAction {
    fn from(_: &ToggleAutoSync) -> Self {
        Self::ToggleAutoSync
    }
}

impl From<&SyncKeepLocal> for UiAction {
    fn from(_: &SyncKeepLocal) -> Self {
        Self::SyncKeepLocal
    }
}

impl From<&SyncKeepRemote> for UiAction {
    fn from(_: &SyncKeepRemote) -> Self {
        Self::SyncKeepRemote
    }
}
