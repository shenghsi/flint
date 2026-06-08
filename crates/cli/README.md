# Cli

## Testing

You can test your changes to the `cli` crate by first building the main flint binary:

```
cargo build -p flint
```

And then building and running the `cli` crate with the following parameters:

```
 cargo run -p cli -- --flint ./target/debug/flint.exe
```
