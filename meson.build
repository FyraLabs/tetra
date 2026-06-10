project(
  'tetra',
  'cpp',
  version : '0.0.1',
  default_options : [
    'cpp_std=c++23,c++20',   # prefer C++23, fall back to C++20
    'buildtype=release',
    'warning_level=3',
    'b_lto=true',
    'b_lto_mode=thin',
    'b_thinlto_cache=true',
  ],
  license : 'LGPLv3',
)

cpp = meson.get_compiler('cpp')

# Try to find mold on the system
mold_prog = find_program('mold', required : false)

if mold_prog.found()
  # Add -fuse-ld=mold to all C++ link arguments
  add_project_link_arguments('-fuse-ld=mold', language : 'cpp')
  message('Using mold linker')
else
  message('mold not found, using default linker')
endif

# --- Dependencies ---
fmt_dep = dependency('fmt', version : '>=10.0')
spdlog_dep = dependency('spdlog', version : '>=1.12', fallback : ['spdlog', 'spdlog_dep'])
threads_dep = dependency('threads')

# --- Configuration header ---
conf_data = configuration_data()
conf_data.set('VERSION', meson.project_version())
conf_data.set('PROJECT_NAME', '"' + meson.project_name() + '"')

config_h = configure_file(
  input : 'config.h.in',
  output : 'config.h',
  configuration : conf_data,
)

public_inc = include_directories('include', is_system : true)

config_inc = include_directories('.')

subdir('src')
subdir('tests')
