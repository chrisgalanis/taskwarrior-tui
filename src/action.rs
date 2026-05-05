#[derive(Clone, PartialEq, Eq, Debug, Copy)]
pub enum Action {
  Report,
  Filter,
  Add,
  AddLinear,
  Annotate,
  Subprocess,
  Log,
  Modify,
  HelpPopup,
  ContextMenu,
  ReportMenu,
  Jump,
  DeletePrompt,
  UndoPrompt,
  DonePrompt,
  Error,
}
