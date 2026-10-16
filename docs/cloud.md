# AutoSave in synced cloud folders

> **Last reviewed:** 2026-10-16 · **Last updated:** 2026-10-16 · **Change:** new; documents AutoSave through iCloud Drive, Google Drive and OneDrive sync folders · **Target:** Microsoft Word (Microsoft 365) for Mac 16.113.4

WordCraft uses the desktop sync clients' ordinary folders. It does not connect to a cloud API. A file saved in one of these folders is uploaded by the installed client and becomes available on the person's other devices.

## Folder discovery

On macOS, WordCraft checks `~/Library/Mobile Documents/com~apple~CloudDocs` for iCloud Drive, each `GoogleDrive-<account>` and `OneDrive-<account>` under `~/Library/CloudStorage`, then older `~/Google Drive` and `~/OneDrive` folders. On Windows, iCloud Drive is `%USERPROFILE%\iCloudDrive`. Google Drive may be a drive letter whose root contains `My Drive` (including localized client names), or a mirrored profile folder. OneDrive uses `%OneDriveConsumer%`, `%OneDriveCommercial%` or `%OneDrive%`. On Linux and other systems there is no assumed client path; `WORDCRAFT_CLOUD_ICLOUD`, `WORDCRAFT_CLOUD_GDRIVE` and `WORDCRAFT_CLOUD_ONEDRIVE` can name folders. The browser build has no folders to discover.

The desktop host discovers roots once. `file.cloud={}` reports provider, root, current path, conflict state and discovered roots. Provider ids are `icloud`, `gdrive` and `onedrive`. iCloud-only files must be downloaded by the sync client before opening.

## Save a new document

The Save to Cloud dialog asks where AutoSave should keep a new document. `file.saveToCloud` accepts optional `provider` (`icloud`, `gdrive` or `onedrive`) and `name` (without extension). With no provider, the first discovered root is chosen in iCloud Drive, Google Drive, OneDrive order. The file is written as a new `.docx` in that provider's documents folder, using a numbered name when needed.

This is the same basic model Word uses when a new document is saved to OneDrive: Word chooses a synced location and its client handles upload. WordCraft does not provide Microsoft's service integration or coauthoring features.

## Conflict protection

WordCraft keeps a disk stamp with size, modification time and a fast byte digest. AutoSave calls `file.save` with `ifUnchanged`. If another device or program changed the bytes, or the file is missing because it was moved or deleted, AutoSave does not overwrite it. It records `changed` or `missing` and pauses. A timestamp-only touch with identical bytes is accepted.

The Save Conflict dialog offers **Keep both**, **Keep this version**, **Open the other version**, **Decide later**, and **Save it here again**. `file.resolveConflict` takes `{"keep":"both"}`, `{"keep":"mine"}` or `{"keep":"theirs"}`. `both` writes `Name (conflict copy).docx` beside the other version and edits the copy. `mine` overwrites the path. `theirs` reopens the file on disk and cannot reopen a missing file. When the file is missing, the dialog offers only **Save it here again** (`mine`, which writes the document back to its original path) and **Decide later**.

## Limits

The feature depends on a compatible sync client being installed and signed in. There is no cloud API, server-side merge or real-time coauthoring. WordCraft has no cloud version-history browser; service retention is outside the application.

## Revision history

| Date | Change |
|---|---|
| 2026-10-16 | Added cloud-folder discovery, Save to Cloud, disk-stamp conflict protection and resolution commands. |
