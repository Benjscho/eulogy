use eulogy::AsyncDrop;

struct Res;

impl AsyncDrop for Res {
    async fn async_drop(self) {}
}

#[derive(AsyncDrop)]
enum Foo {
    Bar { a: Res },
    Baz {
        #[eulogy(after = [a])]
        b: Res,
    },
}

fn main() {}
