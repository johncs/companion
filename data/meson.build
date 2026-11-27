desktop_conf = configuration_data()
desktop_conf.set('app_id', app_id)

desktop_in = configure_file(
  input: app_id + '.desktop.in',
  output: app_id + '.desktop.in',
  configuration: desktop_conf,
)

desktop_file = i18n.merge_file(
  input: desktop_in,
  output: app_id + '.desktop',
  type: 'desktop',
  po_dir: '../po',
  install: true,
  install_dir: get_option('datadir') / 'applications',
)

desktop_utils = find_program('desktop-file-validate', required: false)
if desktop_utils.found()
  test('Validate desktop file', desktop_utils, args: [desktop_file])
endif

metainfo_conf = configuration_data()
metainfo_conf.set('app_id', app_id)

metainfo_in = configure_file(
  input: app_id + '.metainfo.xml.in',
  output: app_id + '.metainfo.xml.in',
  configuration: metainfo_conf,
)

metainfo_file = i18n.merge_file(
  input: metainfo_in,
  output: app_id + '.metainfo.xml',
  type: 'xml',
  po_dir: '../po',
  install: true,
  install_dir: get_option('datadir') / 'metainfo',
)

appstreamcli = find_program('appstreamcli', required: false)
if appstreamcli.found()
  test(
    'Validate metainfo file',
    appstreamcli,
    args: ['validate', '--no-net', '--explain', metainfo_file],
  )
endif

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
