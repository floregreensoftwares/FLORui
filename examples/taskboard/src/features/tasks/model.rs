//! What a task is, and a deterministic set of them to fill the desk with.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Status {
    Todo,
    Doing,
    Blocked,
    Done,
}

impl Status {
    pub const ALL: [Status; 4] = [Status::Todo, Status::Doing, Status::Blocked, Status::Done];

    /// A shorter name, for a key too small for the longer one.
    pub fn key_label(self) -> &'static str {
        match self {
            Status::Todo => "Todo",
            other => other.label(),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Status::Todo => "To do",
            Status::Doing => "Doing",
            Status::Blocked => "Blocked",
            Status::Done => "Done",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Priority {
    Low,
    Normal,
    High,
    Urgent,
}

impl Priority {
    pub const ALL: [Priority; 4] = [
        Priority::Low,
        Priority::Normal,
        Priority::High,
        Priority::Urgent,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Priority::Low => "Low",
            Priority::Normal => "Normal",
            Priority::High => "High",
            Priority::Urgent => "Urgent",
        }
    }

    /// The priority at detent `step` of a knob, the lowest at 0; a step past the
    /// last one is the last.
    pub fn from_step(step: usize) -> Priority {
        Priority::ALL[step.min(Priority::ALL.len() - 1)]
    }

    pub fn step(self) -> usize {
        Priority::ALL
            .iter()
            .position(|&priority| priority == self)
            .unwrap_or(0)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    pub id: u32,
    pub title: String,
    pub notes: String,
    pub status: Status,
    pub priority: Priority,
    /// Days from today until it is due; negative when it is late.
    pub due_in_days: i32,
}

impl Task {
    /// The line under the title: when it is due.
    pub fn due_text(&self) -> String {
        match self.due_in_days {
            0 => "due today".to_string(),
            1 => "due tomorrow".to_string(),
            days if days > 1 => format!("due in {days} days"),
            -1 => "1 day late".to_string(),
            days => format!("{} days late", -days),
        }
    }

    pub fn is_late(&self) -> bool {
        self.due_in_days < 0 && self.status != Status::Done
    }
}

const VERBS: [&str; 10] = [
    "Order",
    "Draft",
    "Review",
    "Sand",
    "Oil",
    "File",
    "Measure",
    "Label",
    "Repair",
    "Call about",
];

const THINGS: [&str; 14] = [
    "the brass hinges",
    "the spring invoices",
    "oak offcuts",
    "the lathe belt",
    "a quote for the shelving",
    "the workshop keys",
    "walnut veneer",
    "the delivery schedule",
    "chisel set",
    "the apprentice's notes",
    "varnish samples",
    "the old ledger",
    "clamps and vices",
    "the drawing for the cabinet",
];

const NOTES: [&str; 6] = [
    "Check the stock first, then the price.",
    "Ask Marta before touching the finish.",
    "Needs the long bench; book it for the morning.",
    "Photograph it before and after.",
    "Waiting for the supplier to call back.",
    "Small job, but the client is particular.",
];

/// `count` tasks, always the same ones: a small linear generator picks each
/// title, state, priority and due date, so a test or a screenshot can rely on
/// them.
pub fn sample_tasks(count: usize) -> Vec<Task> {
    let mut state: u32 = 0x2545_f491;
    let mut next = move || {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        state >> 8
    };
    (0..count)
        .map(|index| {
            let verb = VERBS[next() as usize % VERBS.len()];
            let thing = THINGS[next() as usize % THINGS.len()];
            let status = match next() % 10 {
                0..=3 => Status::Todo,
                4..=6 => Status::Doing,
                7 => Status::Blocked,
                _ => Status::Done,
            };
            Task {
                id: index as u32 + 1,
                title: format!("{verb} {thing}"),
                notes: NOTES[next() as usize % NOTES.len()].to_string(),
                status,
                priority: Priority::ALL[next() as usize % Priority::ALL.len()],
                due_in_days: (next() % 30) as i32 - 6,
            }
        })
        .collect()
}
