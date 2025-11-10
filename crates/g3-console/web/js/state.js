// State management for G3 Console
const state = {
    theme: 'dark',
    lastWorkspace: null,
    g3BinaryPath: null,
    lastProvider: 'databricks',
    lastModel: 'databricks-claude-sonnet-4-5',

    async load() {
        try {
            const data = await api.getState();
            this.theme = data.theme || 'dark';
            this.lastWorkspace = data.last_workspace;
            this.g3BinaryPath = data.g3_binary_path;
            this.lastProvider = data.last_provider || 'databricks';
            this.lastModel = data.last_model || 'databricks-claude-sonnet-4-5';
            return data;
        } catch (error) {
            console.error('Failed to load state:', error);
            return null;
        }
    },

    async save() {
        try {
            await api.saveState({
                theme: this.theme,
                last_workspace: this.lastWorkspace,
                g3_binary_path: this.g3BinaryPath,
                last_provider: this.lastProvider,
                last_model: this.lastModel
            });
        } catch (error) {
            console.error('Failed to save state:', error);
        }
    },

    setTheme(theme) {
        this.theme = theme;
        document.body.className = theme;
        this.save();
    },

    updateLaunchDefaults(workspace, provider, model, binaryPath) {
        this.lastWorkspace = workspace;
        this.lastProvider = provider;
        this.lastModel = model;
        if (binaryPath) this.g3BinaryPath = binaryPath;
        this.save();
    }
};

// Expose to window for global access
window.state = state;
