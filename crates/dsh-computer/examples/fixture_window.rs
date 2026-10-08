#[cfg(windows)]
#[path = "../src/fixture.rs"]
mod fixture;

fn main() -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        println!("DSH Computer Test: isolated EDIT, masked password, and Increment counter button. Close the window to exit.");
        fixture::Fixture::start("DSH Computer Test")?.wait();
    }
    #[cfg(not(windows))]
    anyhow::bail!("The native test fixture requires Windows");
    Ok(())
}
