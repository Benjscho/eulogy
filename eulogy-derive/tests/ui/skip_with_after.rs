use eulogy::AsyncDrop;

struct Res;

impl AsyncDrop for Res {
    async fn async_drop(self) {}
}

#[derive(AsyncDrop)]
struct Foo {
    b: Res,
    #[eulogy(skip, after = [b])]
    a: Res,
}

fn main() {}
