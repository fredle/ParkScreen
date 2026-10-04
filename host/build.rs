fn main() {
    // Embed the application icon (shown in Explorer, the taskbar and Add/Remove Programs).
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/parkscreen.ico");
        res.set("ProductName", "ParkScreen");
        res.set("FileDescription", "ParkScreen");
        res.set("CompanyName", "FBL Consulting Ltd");
        if let Err(e) = res.compile() {
            println!("cargo:warning=could not embed the icon: {e}");
        }
    }
    println!("cargo:rerun-if-changed=assets/parkscreen.ico");
}
