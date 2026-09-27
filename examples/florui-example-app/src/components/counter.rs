use florui::prelude::*;
use florui_reactive::{use_memo, use_signal};

stylesheet!("./counter.css");

#[component]
pub fn Counter() -> Element {
    let count = use_signal(|| 0);
    let clicked = count.clone();
    let doubled = use_memo(count.get(), |n| n * 2);

    view! {
        <div class="counter">
            <span class="count">{count.get().to_string()}</span>
            <span class="doubled">{format!("x2 = {doubled}")}</span>
            <button class="increment" onclick={move || clicked.set(clicked.get() + 1)}>
                {"+1"}
            </button>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use florui_reactive::ComponentScope;
    use florui_style::Arena;

    use super::*;

    fn text_of_class(arena: &Arena, class: &str) -> String {
        let node = arena
            .find(|a, id| a.classes(id).iter().any(|c| c == class))
            .unwrap_or_else(|| panic!("no node with class {class:?}"));
        arena.text_content(node).to_string()
    }

    fn render(scope: &ComponentScope) -> Arena {
        let tree = scope.render(|| Counter(CounterProps {}));
        Arena::build(&tree)
    }

    fn click(arena: &Arena) {
        let button = arena.find(|a, id| a.tag(id) == "button").unwrap();
        arena.handler(button, "click").unwrap().call(&Event::new());
    }

    #[test]
    fn starts_at_zero_and_survives_a_render_with_no_click() {
        let (scope, _dirty) = ComponentScope::new();
        assert_eq!(text_of_class(&render(&scope), "count"), "0");
        assert_eq!(
            text_of_class(&render(&scope), "count"),
            "0",
            "use_signal's initializer must not re-run and reset the count on a later render"
        );
    }

    #[test]
    fn a_click_increments_and_the_next_render_keeps_it() {
        let (scope, _dirty) = ComponentScope::new();
        let arena = render(&scope);
        assert_eq!(text_of_class(&arena, "count"), "0");

        click(&arena);
        let arena = render(&scope);
        assert_eq!(text_of_class(&arena, "count"), "1");

        let arena = render(&scope);
        assert_eq!(
            text_of_class(&arena, "count"),
            "1",
            "the count must persist without another click"
        );

        click(&arena);
        let arena = render(&scope);
        assert_eq!(text_of_class(&arena, "count"), "2");
    }

    #[test]
    fn a_click_marks_the_scope_dirty() {
        let (scope, dirty) = ComponentScope::new();
        let arena = render(&scope);
        assert!(
            !dirty.get(),
            "a render with no click has nothing to notify a host about"
        );
        click(&arena);
        assert!(dirty.get());
    }

    #[test]
    fn the_doubled_memo_tracks_the_count() {
        let (scope, _dirty) = ComponentScope::new();
        let arena = render(&scope);
        assert_eq!(text_of_class(&arena, "doubled"), "x2 = 0");

        click(&arena);
        let arena = render(&scope);
        assert_eq!(text_of_class(&arena, "doubled"), "x2 = 2");

        click(&arena);
        let arena = render(&scope);
        assert_eq!(text_of_class(&arena, "doubled"), "x2 = 4");
    }
}
