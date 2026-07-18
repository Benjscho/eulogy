use eulogy::AsyncDrop;

struct Res;

impl AsyncDrop for Res {
    async fn async_drop(self) {}
}

#[derive(AsyncDrop)]
struct Foo {
    child: Res,
    #[eulogy(after = [chld])]
    parent: Res,
}

fn main() {}
