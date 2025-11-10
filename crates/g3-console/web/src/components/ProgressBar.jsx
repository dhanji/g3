import React from 'react'

function ProgressBar({ instanceType, durationSecs }) {
  const formatDuration = (secs) => {
    const hours = Math.floor(secs / 3600)
    const minutes = Math.floor((secs % 3600) / 60)
    const seconds = secs % 60
    
    if (hours > 0) {
      return `${hours}h ${minutes}m ${seconds}s`
    } else if (minutes > 0) {
      return `${minutes}m ${seconds}s`
    } else {
      return `${seconds}s`
    }
  }

  return (
    <div className="space-y-2">
      <div className="flex justify-between text-sm text-gray-600 dark:text-gray-400">
        <span>Duration: {formatDuration(durationSecs)}</span>
        {instanceType === 'single' && <span>Running...</span>}
      </div>
      <div className="hero-progress">
        <div
          className="hero-progress-bar"
          style={{ width: '100%' }}
        />
      </div>
    </div>
  )
}

export default ProgressBar
