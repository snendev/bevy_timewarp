#[derive(Debug, PartialEq)]
pub enum TimewarpError {
    FrameTooOld,
    FrameTooOldSnapped,
}
