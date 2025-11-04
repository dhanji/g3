// API client for G3 Console
const API_BASE = '/api';

const api = {
    // Get all instances
    async getInstances() {
        const response = await fetch(`${API_BASE}/instances`);
        if (!response.ok) throw new Error('Failed to fetch instances');
        return response.json();
    },

    // Get single instance details
    async getInstance(id) {
        const response = await fetch(`${API_BASE}/instances/${id}`);
        if (!response.ok) throw new Error('Failed to fetch instance');
        return response.json();
    },

    // Get instance logs
    async getInstanceLogs(id) {
        const response = await fetch(`${API_BASE}/instances/${id}/logs`);
        if (!response.ok) throw new Error('Failed to fetch logs');
        return response.json();
    },

    // Launch new instance
    async launchInstance(data) {
        const response = await fetch(`${API_BASE}/instances/launch`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(data)
        });
        if (!response.ok) {
            // Try to extract error message from response
            try {
                const errorData = await response.json();
                throw new Error(errorData.message || errorData.error || 'Failed to launch instance');
            } catch (e) {
                throw new Error(`Failed to launch instance (${response.status})`);
            }
        }
        return response.json();
    },

    // Kill instance
    async killInstance(id) {
        const response = await fetch(`${API_BASE}/instances/${id}/kill`, {
            method: 'POST'
        });
        if (!response.ok) throw new Error('Failed to kill instance');
        return response.json();
    },

    // Restart instance
    async restartInstance(id) {
        const response = await fetch(`${API_BASE}/instances/${id}/restart`, {
            method: 'POST'
        });
        if (!response.ok) throw new Error('Failed to restart instance');
        return response.json();
    },

    // Get console state
    async getState() {
        const response = await fetch(`${API_BASE}/state`);
        if (!response.ok) throw new Error('Failed to fetch state');
        return response.json();
    },

    // Save console state
    async saveState(state) {
        const response = await fetch(`${API_BASE}/state`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(state)
        });
        if (!response.ok) throw new Error('Failed to save state');
        return response.json();
    }
};
