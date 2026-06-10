test_exe = executable(
  'test_engine',
  'test_engine.cpp',
  include_directories : [public_inc],
  dependencies : [fmt_dep, threads_dep],
  link_with : tetra_exe,   # or link_with : myapp_lib if you build a library
)

test('engine tests', test_exe)
