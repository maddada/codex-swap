//! Deterministic, per-thread I/O failures; never compiled into the CLI.
use anyhow::{Result, bail};
use std::{
    cell::RefCell,
    collections::VecDeque,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Point {
    BeforeCommit,
    AfterCommit,
    RollbackRemove,
}

struct Faults {
    path: PathBuf,
    pending: VecDeque<Point>,
    commits: Vec<Vec<u8>>,
}

thread_local! {
    static FAULTS: RefCell<Option<Faults>> = const { RefCell::new(None) };
}

pub(crate) struct Guard;

pub(crate) fn inject(path: &Path, points: &[Point]) -> Guard {
    FAULTS.with(|state| {
        let mut state = state.borrow_mut();
        assert!(state.is_none(), "nested fault injection");
        *state = Some(Faults {
            path: path.to_owned(),
            pending: points.iter().copied().collect(),
            commits: Vec::new(),
        });
    });
    Guard
}

impl Guard {
    pub(crate) fn commits(&self) -> Vec<Vec<u8>> {
        FAULTS.with(|state| {
            let state = state.borrow();
            let faults = state.as_ref().unwrap();
            assert!(faults.pending.is_empty(), "failure point was not reached");
            faults.commits.clone()
        })
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        FAULTS.with(|state| *state.borrow_mut() = None);
    }
}

pub(crate) fn check(path: &Path, point: Point) -> Result<()> {
    let fail = FAULTS.with(|state| {
        let mut state = state.borrow_mut();
        let Some(faults) = state.as_mut().filter(|faults| faults.path == path) else {
            return false;
        };
        if point == Point::AfterCommit {
            faults.commits.push(std::fs::read(path).unwrap());
        }
        if faults.pending.front() == Some(&point) {
            faults.pending.pop_front();
            true
        } else {
            false
        }
    });
    if fail {
        bail!("injected {point:?} failure");
    }
    Ok(())
}
