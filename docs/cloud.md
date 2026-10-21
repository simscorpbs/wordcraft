# AutoSave in synced cloud folders

> **Last reviewed:** 2026-10-21 · **Last updated:** 2026-10-21 · **Change:** AutoSave is on for every saved file, local or in the cloud, and remembered per file · **Target:** Microsoft Word (Microsoft 365) for Mac 16.113.4

WordCraft uses the desktop sync clients' ordinary folders. It does not connect to a cloud API. A file saved in one of these folders is uploaded by the installed client and becomes available on the person's other devices.

## Folder discovery

On macOS, WordCraft checks `~/Library/Mobile Documents/com~apple~CloudDocs` for iCloud Drive, each `GoogleDrive-<account>` and `OneDrive-<account>` under `~/Library/CloudStorage`, then older `~/Google Drive` and `~/OneDrive` folders. On Windows, iCloud Drive is `%USERPROFILE%\iCloudDrive`. Google Drive may be a drive letter whose root contains `My Drive` (including localized client names), or a mirrored profile folder. OneDrive uses `%OneDriveConsumer%`, `%OneDriveCommercial%` or `%OneDrive%`. On Linux and other systems there is no assumed client path; `WORDCRAFT_CLOUD_ICLOUD`, `WORDCRAFT_CLOUD_GDRIVE` and `WORDCRAFT_CLOUD_ONEDRIVE` can name folders. The browser build has no folders to discover.

The desktop host discovers roots once. `file.cloud={}` reports provider, root, current path, conflict state and discovered roots. Provider ids are `icloud`, `gdrive` and `onedrive`. iCloud-only files must be downloaded by the sync client before opening.

## Save a new document

The Save to Cloud dialog asks where AutoSave should keep a new document. `file.saveToCloud` accepts optional `provider` (`icloud`, `gdrive` or `onedrive`) and `name` (without extension). With no provider, the first discovered root is chosen in iCloud Drive, Google Drive, OneDrive order. The file is written as a new `.docx` in that provider's documents folder, using a numbered name when needed.

This is the same basic model Word uses when a new document is saved to OneDrive: Word chooses a synced location and its client handles upload. WordCraft does not provide Microsoft's service integration or coauthoring features.

## Conflict protection
## Closing and switching documents

With AutoSave enabled, an edited document closes, opens another document or creates a new document after WordCraft writes the pending changes with `file.save` and {"ifUnchanged":true}. The action continues without a Save Changes prompt. If another device changed the file, the Save Conflict dialog stays open and the document remains open. If Save Changes was already visible, a successful AutoSave closes that prompt and continues the pending action. With AutoSave disabled, the existing Save Changes behaviour remains.

## AutoSave for each file

AutoSave is on by default for every document whose file exists on disk in a format that keeps everything (`.docx`, `.docm`, `.dotx`, `.dotm`, `.odt`, `.rtf`, and WordCraft's own `.json` and `.xml`). This covers local files as well as files in iCloud Drive, Google Drive or OneDrive folders, and an opened file is covered at once, without a save first. Word turns AutoSave on by default only for files stored in OneDrive or SharePoint. WordCraft also covers local files because the project owner decided so. The disk stamp read when the file is opened protects a local file in the same way as a cloud file.

The switch is still off, with the reason in its tooltip, for a new document that has never been saved, a previous version opened from the history, a format that does not keep everything, the browser, and a file with a conflict waiting. A file opened from bytes, or a recovered document, needs an explicit save before AutoSave writes it, because WordCraft has no disk stamp to check first.

A write that fails, for example because another program holds the file, does not turn AutoSave off. AutoSave pauses and tries again after 5 seconds, then waits twice as long after each further failure, up to one minute. The status bar shows the error while writes keep failing, and the message goes away after the next successful write. The remembered choice for the file does not change.

Each file remembers its switch, as Word remembers it for each document. Turning AutoSave off for a file is remembered when the file is opened again, including in later runs. Turning it back on forgets that choice. The choices are kept in `<settings_dir>/autosave.json`, never in the document, so remembering one does not change the file's bytes. Paths are normalized: on Windows the `\\?\` prefix of canonical paths is removed, separators become `\` and letter case is ignored, and letter case is also ignored on macOS. The file keeps at most 2000 entries, drops the oldest first, is read only up to 1 MiB and is written atomically through a temporary file. Renaming a file (`file.rename`) keeps its choice. Save As, Save to Cloud and Keep both give the new file the switch the document had. The browser build keeps no choices.

File › Options › Save › **Turn on AutoSave by default** sets the default for files with no choice of their own (`file.options` with `{"autosaveByDefault": bool}`, saved with the app's settings as `editing.autosaveByDefault`). It is on by default. When it is off, a file is AutoSaved only after the user turns the switch on for it, and that choice is remembered for the file too.

`file.autosave` with `{"value": bool}` (or `{}` to toggle) sets the open document's switch and remembers it for the file, exactly as the title bar switch does. In the app it fails, with the reason, when AutoSave cannot cover the document. To read the state without changing it, use `file.cloud={}`. Its `autosave` object has `on` (the switch), `default` and `remembered` (`true`, `false`, or `null` when the file follows the default). In the app it also has `covers` (whether AutoSave writes the file), `blocked` (the reason it cannot, or `null`) and `failing` (the last write error while writes keep failing, or `null`).

File › Home and Open list each file once in Recent, using the same path rules without reading the disk. On Windows, `C:\Docs\A.docx` and `c:/docs/a.docx` are one entry.

## Version history

The desktop app keeps a local history for each saved document under `<settings_dir>/versions/<path hash>/`. Every explicit save creates a version. AutoSave writes are coalesced into one version for each ten-minute window, measured from that window's start. Each entry stores the exact saved bytes and an `index.json` record with `id`, `savedAt` (UTC), `author`, `size`, `kind` (`save`, `autosave` or `restore`) and `windowStart`. History is written in the background. Pruning keeps all versions from the last 24 hours, one per day through 30 days, and at most 100 versions or 500 MiB per document.

`file.versions={}` lists versions newest first and returns `{id,savedAt,author,size,kind}`. `file.versions={"open":"id"}` opens an older version as a separate unsaved document named `Name (version <local date time>)`. `file.versions={"restore":"id"}` replaces the current content in one Undo step; AutoSave records that write as a `restore` version. The legacy `{"save":"label"}` and numeric `{"restore":index}` forms remain available for compatibility.

File > Info > Version History and the document name in the title bar open the right-hand history pane. Versions are grouped by local day and show local time and author. The pane offers Open version and Restore. An opened older version shows a Previous version bar with Restore and Save a copy.

WordCraft keeps a disk stamp with size, modification time and a fast byte digest. AutoSave calls `file.save` with `ifUnchanged`. If another device or program changed the bytes, or the file is missing because it was moved or deleted, AutoSave does not overwrite it. It records `changed` or `missing` and pauses. A timestamp-only touch with identical bytes is accepted.

The Save Conflict dialog offers **Keep both**, **Keep this version**, **Open the other version**, **Decide later**, and **Save it here again**. `file.resolveConflict` takes `{"keep":"both"}`, `{"keep":"mine"}` or `{"keep":"theirs"}`. `both` writes `Name (conflict copy).docx` beside the other version and edits the copy. `mine` overwrites the path. `theirs` reopens the file on disk and cannot reopen a missing file. When the file is missing, the dialog offers only **Save it here again** (`mine`, which writes the document back to its original path) and **Decide later**.

## Limits

The feature depends on a compatible sync client being installed and signed in. There is no cloud API, server-side merge or real-time coauthoring. The version history described above is local to the desktop profile, is not synchronized between devices and is unavailable in the browser. iCloud Drive, Google Drive and OneDrive keep their own server-side histories; WordCraft does not browse or merge those histories.

## Revision history

| Date | Change |
|---|---|
| 2026-10-21 | AutoSave covers local files without a save first, remembers each file's switch in the settings folder, and adds the Turn on AutoSave by default option. A failed write is retried instead of turning AutoSave off; `file.cloud` reports the AutoSave state; Recent lists each file once. |
| 2026-10-17 | Added Word-style close and switch behaviour, immediate AutoSave coverage for synced folders and local version history commands. |
| 2026-10-16 | Added cloud-folder discovery, Save to Cloud, disk-stamp conflict protection and resolution commands. |
