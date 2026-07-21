fn main() {
    if std::env::var_os("CARGO_FEATURE_POLKIT").is_none() {
        return;
    }

    let library = pkg_config::Config::new()
        .atleast_version("127")
        .probe("polkit-agent-1")
        .expect(
            "the polkit feature requires polkit-agent-1 development files; install polkit-devel",
        );
    let mut build = cc::Build::new();
    // libpolkit-agent intentionally marks its listener API unstable. Tetra
    // isolates that API to one C bridge and acknowledges the upstream contract.
    build.define("POLKIT_AGENT_I_KNOW_API_IS_SUBJECT_TO_CHANGE", None);
    build.file("native/tetra-polkit-agent.c");
    for path in library.include_paths {
        build.include(path);
    }
    build.compile("tetra-polkit-agent");
}
