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
