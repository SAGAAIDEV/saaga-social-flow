//! Turning a raw reading of where something is into a camera move worth
//! watching.
//!
//! This is the half of tracking that has nothing to do with *what* is being
//! tracked. A face detector and a pointer sensor produce completely different
//! kinds of reading — one is a model's guess with a confidence, the other is
//! an exact fact from the window server — but the problem of getting from a
//! stream of readings to a framing that does not twitch, whip, or chase a
//! false positive is the same problem, and it is solved once here.
//!
//! It lived in [`crate::face`] until there was a second consumer. Nothing
//! about it was face-specific then either — [`smooth::Smoother`] depends on
//! [`Anchor`](crate::region::framing::Anchor) and on nothing else, no
//! MediaPipe and no Core Video — but leaving it there would have meant
//! [`crate::pointer`] reaching through `face::` for arithmetic that has no
//! faces in it, which reads as a dependency that is not real.
//!
//! ## Layout of this module
//!
//! | file | responsibility |
//! |---|---|
//! | [`smooth`] | deadband, glide, speed clamp, jump confirmation, hold on loss |
//! | [`glide`] | one eased scalar, for a value with no detector behind it |

pub mod glide;
pub mod smooth;
