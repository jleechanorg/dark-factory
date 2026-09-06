//! Session-health watchdogs. Bead rev-4ou1z adds the first: a Gemini quota
//! reset watchdog that auto-pokes a paused coder pane once its quota window
//! has reset, instead of burning the transient-retry budget on immediate
//! respawns that hit the same quota wall.

pub mod quota_watchdog;
