//! The tasks' own logic, with no window: what a task says about itself, which
//! tasks a filter lets through, and the counts. Plain data in, plain data out.

use taskboard::features::tasks::{
    Filter, Priority, Status, Task, counts, matches, sample_tasks, visible,
};

fn task(id: u32, title: &str, status: Status, priority: Priority, due: i32) -> Task {
    Task {
        id,
        title: title.to_string(),
        notes: String::new(),
        status,
        priority,
        due_in_days: due,
    }
}

#[test]
fn the_sample_tasks_are_always_the_same_ones() {
    let first = sample_tasks(200);
    let again = sample_tasks(200);
    assert_eq!(first, again);
    assert_eq!(first.len(), 200);
    assert_eq!(first[0].id, 1);
    assert_eq!(first[199].id, 200, "ids count up from one");
    assert!(
        first.iter().any(|t| t.status == Status::Done)
            && first.iter().any(|t| t.status == Status::Blocked)
            && first.iter().any(|t| t.is_late()),
        "a few of every kind, so the screens have something to show"
    );
    // A shorter run is a prefix of a longer one: the generator does not depend on the count.
    assert_eq!(sample_tasks(50), first[..50]);
}

#[test]
fn a_task_says_when_it_is_due_in_words() {
    let due = |days| task(1, "x", Status::Todo, Priority::Normal, days).due_text();
    assert_eq!(due(0), "due today");
    assert_eq!(due(1), "due tomorrow");
    assert_eq!(due(9), "due in 9 days");
    assert_eq!(due(-1), "1 day late");
    assert_eq!(due(-4), "4 days late");
}

#[test]
fn only_a_task_not_done_can_be_late() {
    assert!(task(1, "x", Status::Todo, Priority::Low, -2).is_late());
    assert!(!task(1, "x", Status::Done, Priority::Low, -2).is_late());
    assert!(
        !task(1, "x", Status::Todo, Priority::Low, 0).is_late(),
        "due today is not late"
    );
}

#[test]
fn a_filter_keeps_what_matches_its_status_priority_and_text() {
    let tasks = vec![
        task(1, "Oil the hinges", Status::Todo, Priority::Low, 3),
        task(2, "Order brass", Status::Doing, Priority::High, 3),
        task(3, "Sand the oak", Status::Doing, Priority::Normal, 3),
        task(4, "File the ledger", Status::Done, Priority::Urgent, 3),
    ];
    let everything = Filter::default();
    assert_eq!(visible(&tasks, &everything), [0, 1, 2, 3]);

    let doing = Filter {
        status: Some(Status::Doing),
        ..Filter::default()
    };
    assert_eq!(visible(&tasks, &doing), [1, 2]);

    let important = Filter {
        min_priority: Priority::High,
        ..Filter::default()
    };
    assert_eq!(visible(&tasks, &important), [1, 3]);

    let text = Filter {
        query: "  OAK ".to_string(),
        ..Filter::default()
    };
    assert_eq!(
        visible(&tasks, &text),
        [2],
        "case and surrounding blanks do not matter"
    );

    let both = Filter {
        status: Some(Status::Doing),
        min_priority: Priority::High,
        query: String::new(),
    };
    assert_eq!(visible(&tasks, &both), [1], "every part has to agree");
}

#[test]
fn the_query_also_looks_in_the_notes() {
    let mut with_notes = task(1, "Order brass", Status::Todo, Priority::Low, 3);
    with_notes.notes = "Ask Marta first".to_string();
    let filter = Filter {
        query: "marta".to_string(),
        ..Filter::default()
    };
    assert!(matches(&with_notes, &filter));
    assert!(!matches(
        &task(2, "Order brass", Status::Todo, Priority::Low, 3),
        &filter
    ));
}

#[test]
fn the_counts_add_up_and_the_completion_is_the_share_done() {
    let tasks = vec![
        task(1, "a", Status::Todo, Priority::Low, -3),
        task(2, "b", Status::Doing, Priority::Low, 3),
        task(3, "c", Status::Blocked, Priority::Low, -1),
        task(4, "d", Status::Done, Priority::Low, -9),
    ];
    let counts = counts(&tasks);
    assert_eq!(
        (counts.todo, counts.doing, counts.blocked, counts.done),
        (1, 1, 1, 1)
    );
    assert_eq!(counts.late, 2, "the done one is not late however old it is");
    assert_eq!(counts.total(), 4);
    assert_eq!(counts.completion(), 0.25);
    assert_eq!(
        taskboard::features::tasks::counts(&[]).completion(),
        0.0,
        "no tasks, nothing done"
    );
}

#[test]
fn the_priority_knob_steps_map_to_priorities_and_back() {
    for (step, priority) in Priority::ALL.iter().enumerate() {
        assert_eq!(Priority::from_step(step), *priority);
        assert_eq!(priority.step(), step);
    }
    assert_eq!(
        Priority::from_step(99),
        Priority::Urgent,
        "past the last is the last"
    );
}
