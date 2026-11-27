cargo = find_program('cargo', required: true)

cargo_options = [
  '--manifest-path', meson.project_source_root() / 'Cargo.toml',
  '--target-dir', meson.project_build_root() / 'target',
  '-p', 'companion-app',
  '--release',
]

if get_option('offline')
  cargo_options += ['--offline']
endif

localedir = get_option('prefix') / get_option('localedir')

custom_target(
  'cargo-build',
  build_by_default: true,
  build_always_stale: true,
  output: 'companion',
  console: true,
  install: true,
  install_dir: get_option('bindir'),
  command: [
    'sh', '-c',
    'COMPANION_LOCALEDIR="' + localedir + '" cargo build ' + ' '.join(cargo_options) + ' && cp ' + (meson.project_build_root() / 'target/release/companion') + ' @OUTPUT@'
  ],
)
