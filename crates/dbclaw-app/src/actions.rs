//! Keyboard actions.

gpui_kit::actions!(
    dbclaw,
    [
        Quit,
        NewConnection,
        NewConsole,
        CloseTab,
        NextTab,
        PrevTab,
        ToggleSidebar,
        SearchTables,
        // Table view
        SaveChanges,
        Refresh,
        FocusFilter,
        DeleteRow,
        InsertRow,
        EditCell,
        SetNull,
        CopyValue,
        CancelEdit,
        NextPage,
        PrevPage,
        // Console
        RunQuery,
        RunAll,
        CancelQuery,
    ]
);
