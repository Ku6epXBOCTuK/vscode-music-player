# music-player

Легковесный музыкальный плеер для разработчика: управление из статус-бара VS
Code, настройки в браузере, трансляция звука в OBS Studio.

## Что умеет

- **Управление из VS Code**: play/pause, переключение плейлистов и треков,
  громкость, название текущего трека в статус-баре
- **Плейлисты**: локальные папки (MP3, FLAC, WAV, AAC, M4A, OGG, с вложенными
  папками) и интернет-радио (MP3/AAC-потоки). Имя и путь плейлиста можно менять,
  плейлисты удаляются, последний удалить нельзя
- **Стриминг в OBS**: бесконечный FLAC-поток по HTTP, подключается как Media
  Source
- **Иконка в трее**: текущий трек в tooltip, открытие настроек
- **Состояние переживает перезапуск**: текущий плейлист, громкость и shuffle
  сохраняются в `config.json` при штатной остановке

## Установка

Бэкенд и расширение собираются из исходников (нужны Rust, Node.js и `just`):

```powershell
just install      # соберёт бэкенд и положит его в ~/bin/vscode-music-player
just ext-install  # соберёт .vsix и поставит расширение в VS Code
```

После `ext-install` перезагрузи окно VS Code. Если бэкенд лежит не рядом с
расширением, укажи путь в настройке `music-player.backendPath`.

`just install` сначала останавливает запущенный сервер через `stop-server`, а не
убивает процесс: только штатная остановка успевает записать состояние.

## Использование

- **Play** в статус-баре запускает бэкенд сам, дальше кнопки управляют треками
- **Настройки**: правый клик по иконке в трее → Open settings, или
  `http://127.0.0.1:45880` в браузере. Папки и радиостанции добавляются и
  редактируются оттуда, изменения сразу попадают в `config.json`
- **Остановка**: кнопка Stop в VS Code, трей → Quit или `just stop-server`

## Подключение в OBS Studio

1. Sources → `+` → **Media Source**
2. Сними галку **Local File**
3. В поле **Input** впиши `http://127.0.0.1:45880/stream`
4. OK — в микшере появится живой канал

Поток бесконечный, переключения треков слышны в эфире сразу, при перезапуске
бэкенда OBS переподключается сам. Для простого прослушивания тот же адрес можно
открыть в VLC или любом другом плеере.

Если транслируешь музыку аудитории (Twitch/YouTube), помни про права: обычные
радиостанции для этого не годятся, бери stream-safe источники (NCS, StreamBeats
и подобные DMCA-free каталоги).

## config.json

Лежит рядом с `music-player-server.exe`, создаётся при первом запуске:

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

- `port` — порт API и потока (в расширении та же настройка: `music-player.port`;
  расширение передаёт порт бэкенду при запуске)
- `volume` — 0.0–1.0
- `shuffle` — случайный порядок внутри локального плейлиста
- Недопустимые значения при загрузке исправляются, сломанные записи плейлистов
  пропускаются, файл переписывается в исправленном виде
- Если JSON повреждён, бэкенд не стартует и печатает причину. Рядом лежат
  `config.json.bak` (предыдущая версия) и `config.json.bak.2`

## Логи

Бэкенд пишет `music-player.log` рядом с `music-player-server.exe`. Тот же текст
идёт в консоль, если она есть, но у отсоединённого процесса файл — единственный
источник. Уровень по умолчанию `info` (`debug` в debug-сборке), переопределяется
переменной `MUSIC_PLAYER_LOG`: `debug`, `info`, `warn`, `error`. Файл ротируется
при 4 МБ в `music-player.log.1`.

## Разработка

```powershell
just                 # список рецептов
just backend         # запустить бэкенд в release из backend/target/release
just backend-debug   # то же с debug-логами
just check           # clippy
just check-stream    # записать 10 секунд /stream и проверить ffmpeg
just status          # /api/now_playing
just logs 100        # хвост лога
just control pause   # play, pause, stop, next_track, next_playlist, prev_playlist, toggle_shuffle
just volume 0.5
```

Логика воспроизведения живёт в `backend/src/player.rs`, HTTP API и настройки — в
`backend/src/main.rs`, страница настроек — `backend/assets/index.html`, команды
и статус-бар — `extension/src/extension.ts`.

## Лицензия

MIT — см. [LICENSE](LICENSE). Автор: Ku6epXBOCTuK.
