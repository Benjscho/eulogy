use eulogy::AsyncDrop;

#[derive(AsyncDrop)]
union Foo {
    a: u32,
}

fn main() {}
