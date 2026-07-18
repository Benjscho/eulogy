use eulogy::AsyncDrop;

struct Res;

impl AsyncDrop for Res {
    async fn async_drop(self) {}
}

#[derive(AsyncDrop)]
struct Foo {
    #[eulogy(after = [child])]
    child: Res,
}

fn main() {}
