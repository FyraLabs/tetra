test_exe = executable(
  'test_engine',
  'test_engine.cpp',
  include_directories : [public_inc],
  dependencies : [fmt_dep, threads_dep],
  # If you have a library target (e.g. tetra_lib) you can link the test against it with
  # link_with : tetra_lib
)

test('engine tests', test_exe)
