// File Browser Component
const fileBrowser = {
    currentPath: '',
    selectedPath: '',
    mode: 'directory', // 'directory' or 'file'
    callback: null,
    
    init() {
        const modal = document.getElementById('file-browser-modal');
        const closeBtn = document.getElementById('file-browser-close');
        const cancelBtn = document.getElementById('file-browser-cancel');
        const selectBtn = document.getElementById('file-browser-select');
        const parentBtn = document.getElementById('file-browser-parent');
        
        closeBtn.addEventListener('click', () => this.close());
        cancelBtn.addEventListener('click', () => this.close());
        selectBtn.addEventListener('click', () => this.select());
        parentBtn.addEventListener('click', () => this.goToParent());
        
        // Close on overlay click
        modal.querySelector('.modal-overlay').addEventListener('click', () => this.close());
    },
    
    async open(options = {}) {
        this.mode = options.mode || 'directory';
        this.callback = options.callback;
        this.currentPath = options.initialPath || '/Users';
        this.selectedPath = '';
        
        // Update title
        const title = this.mode === 'directory' ? 'Select Directory' : 'Select File';
        document.getElementById('file-browser-title').textContent = title;
        
        // Show modal
        document.getElementById('file-browser-modal').classList.remove('hidden');
        
        // Load initial directory
        await this.loadDirectory(this.currentPath);
    },
    
    close() {
        document.getElementById('file-browser-modal').classList.add('hidden');
        this.callback = null;
    },
    
    select() {
        if (this.selectedPath && this.callback) {
            this.callback(this.selectedPath);
        }
        this.close();
    },
    
    async goToParent() {
        const parts = this.currentPath.split('/').filter(p => p);
        if (parts.length > 0) {
            parts.pop();
            const parentPath = '/' + parts.join('/');
            await this.loadDirectory(parentPath);
        }
    },
    
    async loadDirectory(path) {
        const listContainer = document.getElementById('file-browser-list');
        listContainer.innerHTML = '<div class="spinner-container"><div class="spinner"></div><p>Loading...</p></div>';
        
        try {
            const data = await api.browseFilesystem(path, this.mode);
            this.currentPath = data.current_path;
            this.selectedPath = this.mode === 'directory' ? this.currentPath : '';
            
            // Update current path display
            document.getElementById('file-browser-current-path').value = this.currentPath;
            
            // Render items
            this.renderItems(data.entries);
        } catch (error) {
            console.error('Failed to load directory:', error);
            listContainer.innerHTML = `<div class="error-message">Failed to load directory: ${error.message}</div>`;
        }
    },
    
    renderItems(entries) {
        const listContainer = document.getElementById('file-browser-list');
        
        if (entries.length === 0) {
            listContainer.innerHTML = '<div style="padding: 2rem; text-align: center; color: var(--text-secondary);">Empty directory</div>';
            return;
        }
        
        // Sort: directories first, then files, alphabetically
        entries.sort((a, b) => {
            if (a.is_dir !== b.is_dir) {
                return a.is_dir ? -1 : 1;
            }
            return a.name.localeCompare(b.name);
        });
        
        let html = '';
        for (const entry of entries) {
            const icon = entry.is_dir ? '📁' : '📄';
            const className = entry.is_dir ? 'directory' : 'file';
            const isSelected = entry.path === this.selectedPath;
            
            // Only show files if in file mode, always show directories
            if (this.mode === 'file' && !entry.is_dir) {
                html += `
                    <div class="file-browser-item ${className} ${isSelected ? 'selected' : ''}" 
                         data-path="${entry.path}" 
                         data-is-dir="${entry.is_dir}">
                        <span class="file-browser-icon">${icon}</span>
                        <span class="file-browser-name">${entry.name}</span>
                    </div>
                `;
            } else if (entry.is_dir) {
                html += `
                    <div class="file-browser-item ${className} ${isSelected ? 'selected' : ''}" 
                         data-path="${entry.path}" 
                         data-is-dir="${entry.is_dir}">
                        <span class="file-browser-icon">${icon}</span>
                        <span class="file-browser-name">${entry.name}</span>
                    </div>
                `;
            }
        }
        
        listContainer.innerHTML = html;
        
        // Add click handlers
        listContainer.querySelectorAll('.file-browser-item').forEach(item => {
            item.addEventListener('click', () => this.handleItemClick(item));
        });
    },
    
    async handleItemClick(item) {
        const path = item.dataset.path;
        const isDir = item.dataset.isDir === 'true';
        
        if (isDir) {
            // Double-click to navigate into directory
            if (this.selectedPath === path) {
                await this.loadDirectory(path);
            } else {
                // Single click to select directory
                this.selectedPath = path;
                // Update UI
                document.querySelectorAll('.file-browser-item').forEach(i => {
                    i.classList.remove('selected');
                });
                item.classList.add('selected');
            }
        } else {
            // Select file
            this.selectedPath = path;
            // Update UI
            document.querySelectorAll('.file-browser-item').forEach(i => {
                i.classList.remove('selected');
            });
            item.classList.add('selected');
        }
    }
};

// Expose to window
window.fileBrowser = fileBrowser;
