use eulogy::AsyncDrop;

struct Res;

impl AsyncDrop for Res {
    async fn async_drop(self) {}
}

#[derive(AsyncDrop)]
struct Foo {
    #[eulogy(after = [b])]
    a: Res,
    #[eulogy(after = [a])]
    b: Res,
}

fn main() {}
