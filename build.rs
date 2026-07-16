fn main() {
    glib_build_tools::compile_resources(
        &["data"],
        "data/openwave.gresource.xml",
        "openwave.gresource",
    );
}
