import React from 'react'

function StatusBadge({ status }) {
  const getStatusClass = () => {
    switch (status) {
      case 'running':
        return 'hero-badge hero-badge-success'
      case 'completed':
        return 'hero-badge hero-badge-success'
      case 'failed':
        return 'hero-badge hero-badge-error'
      case 'idle':
        return 'hero-badge hero-badge-warning'
      case 'terminated':
        return 'hero-badge hero-badge-error'
      default:
        return 'hero-badge hero-badge-info'
    }
  }

  return (
    <span className={getStatusClass()}>
      {status.toUpperCase()}
    </span>
  )
}

export default StatusBadge
