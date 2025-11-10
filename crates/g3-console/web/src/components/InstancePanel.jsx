import React from 'react'
import StatusBadge from './StatusBadge'
import ProgressBar from './ProgressBar'

function InstancePanel({ instance, onClick, onKill, onRestart }) {
  const { instance: inst, stats, latest_message } = instance

  const handleKill = (e) => {
    e.stopPropagation()
    if (window.confirm('Are you sure you want to kill this instance?')) {
      onKill()
    }
  }

  const handleRestart = (e) => {
    e.stopPropagation()
    onRestart()
  }

  return (
    <div
      onClick={onClick}
      className="hero-card p-6 cursor-pointer"
    >
      <div className="flex justify-between items-start mb-4">
        <div className="flex-1">
          <div className="flex items-center gap-3 mb-2">
            <h3 className="text-lg font-semibold text-gray-900 dark:text-white">
              {inst.workspace.split('/').pop() || 'Unknown'}
            </h3>
            <StatusBadge status={inst.status} />
            <span className="text-sm text-gray-600 dark:text-gray-400">
              {inst.instance_type === 'ensemble' ? 'Coach + Player' : 'Single Agent'}
            </span>
          </div>
          <div className="text-sm text-gray-600 dark:text-gray-400">
            PID: {inst.pid} | Started: {new Date(inst.start_time).toLocaleTimeString()}
          </div>
        </div>
        <div className="flex gap-2">
          {inst.status === 'running' && (
            <button
              onClick={handleKill}
              className="hero-button hero-button-danger text-sm"
            >
              Kill
            </button>
          )}
          {inst.status === 'terminated' && (
            <button
              onClick={handleRestart}
              className="hero-button hero-button-secondary text-sm"
            >
              Restart
            </button>
          )}
        </div>
      </div>

      <ProgressBar
        instanceType={inst.instance_type}
        durationSecs={stats.duration_secs}
      />

      <div className="grid grid-cols-3 gap-4 mt-4">
        <div>
          <div className="text-xs text-gray-600 dark:text-gray-400">Tokens</div>
          <div className="text-lg font-semibold text-gray-900 dark:text-white">
            {stats.total_tokens.toLocaleString()}
          </div>
        </div>
        <div>
          <div className="text-xs text-gray-600 dark:text-gray-400">Tool Calls</div>
          <div className="text-lg font-semibold text-gray-900 dark:text-white">
            {stats.tool_calls}
          </div>
        </div>
        <div>
          <div className="text-xs text-gray-600 dark:text-gray-400">Errors</div>
          <div className="text-lg font-semibold text-gray-900 dark:text-white">
            {stats.errors}
          </div>
        </div>
      </div>

      {latest_message && (
        <div className="mt-4 text-sm text-gray-600 dark:text-gray-400 truncate">
          <strong>Latest:</strong> {latest_message}
        </div>
      )}

      <div className="mt-2 text-xs text-gray-500 dark:text-gray-500">
        {inst.workspace}
      </div>
    </div>
  )
}

export default InstancePanel
