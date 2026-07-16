use eulogy::AsyncDrop;

struct Res;

impl AsyncDrop for Res {
    async fn async_drop(self) {}
}

#[derive(AsyncDrop)]
struct Foo {
    #[eulogy(skip)]
    child: Res,
    #[eulogy(after = [child])]
    parent: Res,
}

fn main() {}
