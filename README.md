# music-player

Легковесный музыкальный плеер для разработчика: управление из статус-бара VS
Code, настройки в браузере, трансляция звука в OBS Studio.

## Что умеет

- **Управление из VS Code**: Play/Pause, переключение плейлистов, громкость,
  название текущего трека — прямо в статус-баре редактора
- **Плейлисты**: локальные папки с музыкой (MP3, FLAC, WAV, AAC, M4A, OGG, с
  вложенными папками) и интернет-радио (MP3/AAC-потоки)
- **Стриминг в OBS**: бесконечный FLAC-поток по HTTP, подключается в OBS Studio
  как Media Source
- **Иконка в трее**: текущий трек в tooltip, открытие настроек, выход
- **Настройки в браузере**: добавление папок и радиостанций без правки файлов

## Установка

### 1. Бэкенд (сервер плеера)

Скопируй `backend.exe` в любую папку, например
`%USERPROFILE%\bin\vscode-music-player\`. При первом запуске рядом появится
`config.json` с настройками по умолчанию.

### 2. Расширение VS Code

1. Возьми файл `music-player-extension-<версия>.vsix`
2. VS Code → Extensions → `...` → **Install from VSIX...** → выбери файл
3. В настройках расширения (`music-player.backendPath`) укажи полный путь к
   `backend.exe`, если он не лежит в папке проекта

## Использование

- Нажми **Play** в статус-баре — расширение само запустит бэкенд
- **Настройки плеера**: правый клик по иконке в трее → Open settings, или открой
  `http://127.0.0.1:45880` в браузере. Добавь папки с музыкой и радиостанции —
  всё сохраняется в `config.json`
- **Выключить плеер**: кнопка Stop в VS Code, или трей → Quit

## Подключение в OBS Studio

1. Sources → `+` → **Media Source**
2. Сними галку **Local File**
3. В поле **Input** впиши: `http://127.0.0.1:45880/stream`
4. OK — в микшере появится живой канал

Поток бесконечный, переключения треков и громкость слышны в эфире сразу. При
перезапуске бэкенда OBS переподключается сам.

Просто послушать (без эфира) поток можно и в VLC: Media → Open Network Stream →
`http://127.0.0.1:45880/stream`.

Если транслируешь музыку аудитории (Twitch/YouTube), помни про права: обычные
радиостанции для этого не годятся — бери stream-safe источники (NCS, StreamBeats
и подобные DMCA-free каталоги).

## Настройки (config.json)

```json
{
  "version": 1,
  "current_playlist_index": 0,
  "volume": 0.8,
  "port": 45880,
  "shuffle": false,
  "playlists": [
    { "name": "Моя музыка", "type": "local_folder", "path": "C:/Music" },
    {
      "name": "Lofi Radio",
      "type": "network_radio",
      "path": "https://play.streamafrica.net/lofiradio"
    }
  ]
}
```

- `port` — порт API и потока (в расширении VS Code та же настройка:
  `music-player.port`; расширение передаёт порт бэкенду при запуске)
- `volume` — 0.0–1.0
- `shuffle` — режим случайного воспроизведения
- Плейлисты удобнее добавлять через панель настроек в браузере
- Значения проверяются при загрузке: недопустимые значения исправляются,
  сломанные записи плейлистов пропускаются, файл переписывается в исправленном
  виде
- Если `config.json` повреждён (невалидный JSON), бэкенд не стартует и печатает
  причину; файл остаётся нетронутым. Рядом лежат резервные копии:
  `config.json.bak` (предыдущая версия) и `config.json.bak.2`

## Logs

The backend writes a detailed log to `music-player.log` next to `backend.exe`
(same folder as `config.json`). The same text goes to the console when one is
attached, so the file is the only source of truth for a detached run.

Default level is `info` (`debug` in debug builds). Override it with the
`MUSIC_PLAYER_LOG` environment variable:

```powershell
$env:MUSIC_PLAYER_LOG = "debug"
.\backend.exe
```

Accepted values: `debug`, `info`, `warn`, `error`. At `debug` the log includes
codec, sample rate and channel count per source, file sizes and decode progress.
Once the log reaches 4 MB it is renamed to `music-player.log.1` and a fresh file
is started.

## Uninstall

Delete the folder holding `backend.exe` (it also holds `config.json` and
`music-player.log`), then remove the extension from VS Code.

## Лицензия

MIT — см. [LICENSE](LICENSE). Автор: Ku6epXBOCTuK.
