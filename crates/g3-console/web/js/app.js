// Main application logic

// Global action handlers
window.handleKill = async function(id) {
    if (!confirm('Are you sure you want to kill this instance?')) return;
    
    // Find the button and show loading state
    const button = event.target;
    const originalText = button.textContent;
    button.disabled = true;
    button.innerHTML = '<span class="spinner" style="width: 1rem; height: 1rem; border-width: 2px; display: inline-block; vertical-align: middle;"></span> Terminating...';
    
    try {
        await api.killInstance(id);
        
        // Show success state
        button.innerHTML = '✓ Terminated';
        button.classList.remove('btn-danger');
        button.classList.add('btn-secondary');
        
        // Refresh after a short delay
        setTimeout(() => {
            router.handleRoute(router.currentRoute);
        }, 1000);
    } catch (error) {
        // Restore button state on error
        button.disabled = false;
        button.textContent = originalText;
        alert('Failed to kill instance: ' + error.message);
    }
};

window.handleRestart = async function(id) {
    // Find the button and show loading state
    const button = event.target;
    const originalText = button.textContent;
    button.disabled = true;
    button.innerHTML = '<span class="spinner" style="width: 1rem; height: 1rem; border-width: 2px; display: inline-block; vertical-align: middle;"></span> Restarting...';
    
    try {
        await api.restartInstance(id);
        
        // Show intermediate states
        button.innerHTML = '<span class="spinner" style="width: 1rem; height: 1rem; border-width: 2px; display: inline-block; vertical-align: middle;"></span> Starting...';
        
        // Wait a bit then show success
        setTimeout(() => {
            button.innerHTML = '✓ Running';
            button.classList.remove('btn-primary');
            button.classList.add('btn-success');
        }, 1500);
        
        // Refresh current view
        setTimeout(() => {
            router.handleRoute(router.currentRoute);
        }, 2500);
    } catch (error) {
        // Restore button state on error
        button.disabled = false;
        button.textContent = originalText;
        alert('Failed to kill instance: ' + error.message);
    }
};

// Modal management
const modal = {
    element: null,
    
    init() {
        this.element = document.getElementById('new-run-modal');
        
        // Close button
        document.getElementById('modal-close').addEventListener('click', () => this.close());
        document.getElementById('cancel-launch').addEventListener('click', () => this.close());
        
        // Close on overlay click
        this.element.querySelector('.modal-overlay').addEventListener('click', () => this.close());
        
        // Form submission
        document.getElementById('launch-form').addEventListener('submit', (e) => {
            e.preventDefault();
            this.handleLaunch();
        });
        
        // File browser buttons - use HTML5 file input
        document.getElementById('browse-workspace').addEventListener('click', () => {
            this.browseDirectory('workspace');
        });
        
        document.getElementById('browse-binary').addEventListener('click', () => {
            this.browseFile('g3-binary-path');
        });
        
        // Provider change updates model options
        document.getElementById('provider').addEventListener('change', (e) => {
            this.updateModelOptions(e.target.value);
        });
    },
    
    browseDirectory(inputId) {
        // Use custom file browser
        fileBrowser.open({
            mode: 'directory',
            initialPath: document.getElementById(inputId).value || '/Users',
            callback: (path) => {
                document.getElementById(inputId).value = path;
            }
        });
    },
    
    browseFile(inputId) {
        // Use custom file browser
        fileBrowser.open({
            mode: 'file',
            initialPath: document.getElementById(inputId).value || '/Users',
            callback: (path) => {
                document.getElementById(inputId).value = path;
            }
        });
    },
    
    open() {
        // Load saved state
        const form = document.getElementById('launch-form');
        if (state.lastWorkspace) {
            form.workspace.value = state.lastWorkspace;
        }
        if (state.g3BinaryPath) {
            form.g3_binary_path.value = state.g3BinaryPath;
        }
        form.provider.value = state.lastProvider || 'databricks';
        this.updateModelOptions(state.lastProvider || 'databricks');
        form.model.value = state.lastModel || 'databricks-claude-sonnet-4-5';
        
        this.element.classList.remove('hidden');
    },
    
    close() {
        this.element.classList.add('hidden');
    },
    
    updateModelOptions(provider) {
        const modelSelect = document.getElementById('model');
        const models = {
            databricks: [
                { value: 'databricks-claude-sonnet-4-5', label: 'databricks-claude-sonnet-4-5' },
                { value: 'databricks-meta-llama-3-1-405b-instruct', label: 'databricks-meta-llama-3-1-405b-instruct' }
            ],
            anthropic: [
                { value: 'claude-3-5-sonnet-20241022', label: 'claude-3-5-sonnet-20241022' },
                { value: 'claude-3-opus-20240229', label: 'claude-3-opus-20240229' }
            ],
            local: [
                { value: 'local-model', label: 'Local Model' }
            ]
        };
        
        modelSelect.innerHTML = '';
        for (const model of models[provider] || []) {
            const option = document.createElement('option');
            option.value = model.value;
            option.textContent = model.label;
            modelSelect.appendChild(option);
        }
    },
    
    async handleLaunch() {
        const form = document.getElementById('launch-form');
        const formData = new FormData(form);
        
        const data = {
            prompt: formData.get('prompt'),
            workspace: formData.get('workspace'),
            provider: formData.get('provider'),
            model: formData.get('model'),
            mode: formData.get('mode'),
            g3_binary_path: formData.get('g3_binary_path') || null
        };
        
        const submitBtn = form.querySelector('button[type="submit"]');
        const modalBody = this.element.querySelector('.modal-body');
        
        try {
            // Show loading state
            submitBtn.disabled = true;
            submitBtn.innerHTML = '<span class="spinner" style="width: 1rem; height: 1rem; border-width: 2px; display: inline-block; vertical-align: middle;"></span> Starting g3 instance...';
            
            const response = await api.launchInstance(data);
            
            // Show intermediate state
            submitBtn.innerHTML = '<span class="spinner" style="width: 1rem; height: 1rem; border-width: 2px; display: inline-block; vertical-align: middle;"></span> Waiting for process...';
            
            // Wait a bit to let the process start
            await new Promise(resolve => setTimeout(resolve, 1500));
            submitBtn.innerHTML = '✓ Instance started!';
            
            // Save state
            state.updateLaunchDefaults(
                data.workspace,
                data.provider,
                data.model,
                data.g3_binary_path
            );
            
            // Close modal and navigate home
            this.close();
            router.navigate('/');
            
            // Reset form
            form.reset();
            submitBtn.disabled = false;
            submitBtn.textContent = 'Start Instance';
        } catch (error) {
            // Display detailed error message in modal
            const errorDiv = document.createElement('div');
            errorDiv.className = 'error-message';
            errorDiv.style.cssText = 'background: #fee; border: 1px solid #fcc; color: #c33; padding: 1rem; margin: 1rem 0; border-radius: 0.5rem;';
            
            let errorMessage = 'Failed to launch instance';
            if (error.message) {
                errorMessage += ': ' + error.message;
            }
            
            // Check for specific error types
            if (error.message && error.message.includes('400')) {
                errorMessage = 'Invalid configuration. Please check that the g3 binary path exists and is executable, and that the workspace directory is valid.';
            } else if (error.message && error.message.includes('500')) {
                errorMessage = 'Server error while launching instance. Check console logs for details.';
            }
            
            errorDiv.textContent = errorMessage;
            
            // Remove any existing error messages
            const existingError = modalBody.querySelector('.error-message');
            if (existingError) existingError.remove();
            
            // Insert error message at the top of modal body
            modalBody.insertBefore(errorDiv, modalBody.firstChild);
            
            submitBtn.disabled = false;
            submitBtn.textContent = 'Start Instance';
        }
    }
};

// Theme toggle
function initTheme() {
    const themeToggle = document.getElementById('theme-toggle');
    
    themeToggle.addEventListener('click', () => {
        const newTheme = state.theme === 'dark' ? 'light' : 'dark';
        state.setTheme(newTheme);
        themeToggle.textContent = newTheme === 'dark' ? '🌙' : '☀️';
    });
    
    // Set initial theme
    document.body.className = state.theme;
    themeToggle.textContent = state.theme === 'dark' ? '🌙' : '☀️';
}

// Initialize app
async function init() {
    // Prevent double initialization
    if (window.g3Initialized) {
        console.log('[App] init() called but already initialized, returning');
        return;
    }
    window.g3Initialized = true;
    console.log('[App] init() starting...');
    
    // Load state
    await state.load();
    
    // Initialize theme
    initTheme();
    
    // Initialize modal
    modal.init();
    
    // Initialize file browser
    fileBrowser.init();
    
    // Expose modal to window for button access
    window.modal = modal;
    
    // New Run button
    document.getElementById('new-run-btn').addEventListener('click', () => {
        modal.open();
    });
    
    // Initialize router
    console.log('[App] About to call router.init()');
    router.init();
    console.log('[App] init() complete');
}

// Simplified initialization - call exactly once when DOM is ready
if (document.readyState === 'loading') {
    // DOM still loading, wait for DOMContentLoaded
    document.addEventListener('DOMContentLoaded', init, { once: true });
} else {
    // DOM already loaded (interactive or complete), init immediately
    init();
}
