# AutoSave in synced cloud folders

> **Last reviewed:** 2026-10-17 · **Last updated:** 2026-10-17 · **Change:** AutoSave closes and switches documents like Word; added local version history · **Target:** Microsoft Word (Microsoft 365) for Mac 16.113.4

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

A document opened from an iCloud Drive, Google Drive or OneDrive folder is eligible for AutoSave immediately. It does not need an explicit save first. A local document outside those folders still needs an explicit save before AutoSave can write it.

## Version history

The desktop app keeps a local history for each saved document under `<settings_dir>/versions/<path hash>/`. Every explicit save creates a version. AutoSave writes are coalesced into one version for each ten-minute window, measured from that window's start. Each entry stores the exact saved bytes and an `index.json` record with `id`, `savedAt` (UTC), `author`, `size`, `kind` (`save`, `autosave` or `restore`) and `windowStart`. History is written in the background. Pruning keeps all versions from the last 24 hours, one per day through 30 days, and at most 100 versions or 500 MiB per document.

`file.versions={}` lists versions newest first and returns `{id,savedAt,author,size,kind}`. `file.versions={"open":"id"}` opens an older version as a separate unsaved document named `Name (version <local date time>)`. `file.versions={"restore":"id"}` replaces the current content in one Undo step; AutoSave records that write as a `restore` version. The legacy `{"save":"label"}` and numeric `{"restore":index}` forms remain available for compatibility.

File > Info > Version History opens the right-hand history pane. Clicking the document name in the title bar (left or right button) opens a small file menu, as Word does for a OneDrive document: the file name, which can be changed there and applied with Enter (`file.rename`); the folder that holds the file, or Not saved; and Version History. The pane lists versions newest first under day headings: Today, Yesterday, or the date (month names in English and Portuguese, an ISO date in other interface languages). Each row shows the local time and the author, with an Open version link; hovering a row shows its full date and time, and right-clicking it offers Open version and Restore. With no versions the pane says why: the document has not been saved yet, the history is empty, or (in the browser) the history is kept by the desktop app.

Before a version is opened or restored, AutoSave writes the current document if it has unsaved changes, so that state is in the history too. An opened version is read only. Commands that would change it, including Undo and Redo, are refused with a status-line message; file commands, the view, moving the insertion point, selecting and copying still work. Its row in the pane is highlighted and has no link, and the title bar reads `Name (version <local date time>) — Previous version`. A Previous version bar above the page shows the date, time and author with **Compare**, **Restore**, **Save a copy** and **Close**.

- **Compare** (`file.compareVersion={}`, same options as `review.compare`) shows the previous version as the original and the current document as the revised one, with the differences as tracked changes. The comparison takes the version's place and stays read only; pressing Compare again shows the version.
- **Restore** first returns to the current document, then runs `file.versions={"restore":"id"}`: the version becomes the document's content in one Undo step, and AutoSave records it.
- **Save a copy** saves the version as a new file and then returns to the current document.
- **Close** (`file.closeVersion={}`) returns to the current document as it was: its unsaved changes, password and AutoSave disk stamp are kept while the version is shown and put back.

WordCraft has one document per window, so the version is shown in the document's own window instead of a separate one. New, Open and Close while a version is shown first return to the current document. Close stops there, as closing the version's window would in Word, and New and Open then save or ask about the document's changes as usual. Quitting the app does the same, so a shown version never hides unsaved work.

WordCraft keeps a disk stamp with size, modification time and a fast byte digest. AutoSave calls `file.save` with `ifUnchanged`. If another device or program changed the bytes, or the file is missing because it was moved or deleted, AutoSave does not overwrite it. It records `changed` or `missing` and pauses. A timestamp-only touch with identical bytes is accepted.

The Save Conflict dialog offers **Keep both**, **Keep this version**, **Open the other version**, **Decide later**, and **Save it here again**. `file.resolveConflict` takes `{"keep":"both"}`, `{"keep":"mine"}` or `{"keep":"theirs"}`. `both` writes `Name (conflict copy).docx` beside the other version and edits the copy. `mine` overwrites the path. `theirs` reopens the file on disk and cannot reopen a missing file. When the file is missing, the dialog offers only **Save it here again** (`mine`, which writes the document back to its original path) and **Decide later**.

## Limits

The feature depends on a compatible sync client being installed and signed in. There is no cloud API, server-side merge or real-time coauthoring. The version history described above is local to the desktop profile, is not synchronized between devices and is unavailable in the browser. iCloud Drive, Google Drive and OneDrive keep their own server-side histories; WordCraft does not browse or merge those histories.

## Revision history

| Date | Change |
|---|---|
| 2026-10-21 | Version History works more like Word: title-bar file menu, Today and Yesterday day headings, read-only previous versions with Compare, Restore, Save a copy and Close, and AutoSave before a version is opened or restored. |
| 2026-10-17 | Added Word-style close and switch behaviour, immediate AutoSave coverage for synced folders and local version history commands. |
| 2026-10-16 | Added cloud-folder discovery, Save to Cloud, disk-stamp conflict protection and resolution commands. |
