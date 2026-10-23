"""Start Yakusu: a local server plus a native window (or your browser)."""
import argparse
import sys
import time
import webbrowser

from .server import serve


def main():
    ap = argparse.ArgumentParser(prog='yakusu', description='Translate Japanese visual novels into English patches.')
    ap.add_argument('--browser', action='store_true', help='open in your web browser instead of an app window')
    ap.add_argument('--port', type=int, default=0)
    args = ap.parse_args()

    app, httpd, url = serve(args.port)
    url_with_token = url

    if not args.browser:
        try:
            import webview  # pywebview: native window using Edge WebView2 on Windows
        except ImportError:
            webview = None
        if webview:
            window = webview.create_window('Yakusu', url_with_token, width=1180, height=820, min_size=(860, 600))

            def pick_folder(title):
                res = window.create_file_dialog(webview.FOLDER_DIALOG)
                return res[0] if res else ''
            app.dialog = pick_folder

            def on_loaded():
                # Dropping a folder on the window opens it (the app window can
                # see full paths; a plain browser can't).
                try:
                    from webview.dom import DOMEventHandler

                    def on_drop(e):
                        files = (e.get('dataTransfer') or {}).get('files') or []
                        path = files[0].get('pywebviewFullPath') if files else None
                        if path:
                            try:
                                app.open_game(path)
                            except Exception as ex:  # shown in the Activity log
                                app.log(f'Error: {ex}')
                    window.dom.document.events.dragover += DOMEventHandler(lambda e: None, True, True)
                    window.dom.document.events.drop += DOMEventHandler(on_drop, True, True)
                except Exception:
                    pass
            window.events.loaded += on_loaded
            webview.start()
            httpd.shutdown()
            return

    print(f'Yakusu is running at {url_with_token}')
    print('Close this window to quit.')
    webbrowser.open(url_with_token)
    try:
        while True:
            time.sleep(3600)
    except KeyboardInterrupt:
        httpd.shutdown()


if __name__ == '__main__':
    sys.exit(main())
