desktop_conf = configuration_data()
desktop_conf.set('app_id', app_id)

desktop_file = configure_file(
  input: app_id + '.desktop.in',
  output: app_id + '.desktop',
  configuration: desktop_conf,
  install: true,
  install_dir: get_option('datadir') / 'applications',
)

metainfo_conf = configuration_data()
metainfo_conf.set('app_id', app_id)

metainfo_file = configure_file(
  input: app_id + '.metainfo.xml.in',
  output: app_id + '.metainfo.xml',
  configuration: metainfo_conf,
  install: true,
  install_dir: get_option('datadir') / 'metainfo',
)

install_data(
  'icons/hicolor/scalable/apps/' + app_id + '.svg',
  install_dir: get_option('datadir') / 'icons/hicolor/scalable/apps',
)

install_data(
  'icons/hicolor/symbolic/apps/' + app_id + '-symbolic.svg',
  install_dir: get_option('datadir') / 'icons/hicolor/symbolic/apps',
)

install_data(
  'icons/hicolor/symbolic/actions/companion-thinking-symbolic.svg',
  install_dir: get_option('datadir') / 'icons/hicolor/symbolic/actions',
)

install_data(
  'icons/hicolor/symbolic/actions/companion-chat-symbolic.svg',
  install_dir: get_option('datadir') / 'icons/hicolor/symbolic/actions',
)

install_data(
  'icons/hicolor/symbolic/actions/companion-model-symbolic.svg',
  install_dir: get_option('datadir') / 'icons/hicolor/symbolic/actions',
)

install_data(
  'icons/hicolor/symbolic/actions/companion-prompt-symbolic.svg',
  install_dir: get_option('datadir') / 'icons/hicolor/symbolic/actions',
)

install_data(
  'icons/hicolor/symbolic/actions/companion-globe-symbolic.svg',
  install_dir: get_option('datadir') / 'icons/hicolor/symbolic/actions',
)
