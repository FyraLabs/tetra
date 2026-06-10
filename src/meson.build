sources = files(
    'main.cpp',
    'userspace.cpp',
    'IOProcesor.cpp',
)

tetra_exe = executable(
    'tetra',
    sources,
    config_h,
    include_directories : [public_inc, config_inc],
    dependencies : [fmt_dep, spdlog_dep, threads_dep],
    install : true,
)
